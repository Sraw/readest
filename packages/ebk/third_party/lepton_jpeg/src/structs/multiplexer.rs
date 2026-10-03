// Modified for the EBK project: see EBK-CHANGES.md at the root of this crate.
//! Implements a multiplexer that reads blocks from a stream from multiple partitions. Each
//! partition can run on it own thread to allow for increased parallelism when processing large images.
//!
//! The writer (left out of this copy) identifies the blocks by partition_id and tries to write in 64K blocks. The file
//! ends up with an interleaved stream of blocks from each partition.
//!
//! The read implementation reads the blocks from the file and sends them to the appropriate worker thread
//! for the partition.

use std::collections::VecDeque;
use std::io::{Cursor, Read};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use super::simple_threadpool::LeptonThreadPool;

use crate::lepton_error::{ExitCode, Result};
use crate::{LeptonError, Metrics};
use crate::{helpers::*, lepton_error::err_exit_code, structs::partial_buffer::PartialBuffer};

/// The message that is sent between the threads
enum Message {
    Eof(usize),
    WriteBlock(usize, Vec<u8>),
}

/// Used by the processor thread to read data in a blocking way.
/// The partition_id is used only to assert that we are only
/// getting the data that we are expecting.
pub struct MultiplexReader {
    /// the multiplexed thread stream we are processing
    partition_id: usize,

    /// the receiver part of the channel to get more buffers
    receiver: Receiver<Message>,

    /// what we are reading. When this returns zero, we try to
    /// refill the buffer if we haven't reached the end of the stream
    current_buffer: Cursor<Vec<u8>>,

    /// once we get told we are at the end of the stream, we just
    /// always return 0 bytes
    end_of_file: bool,
}

impl Read for MultiplexReader {
    /// fast path for reads. If we run out of data, take the slow path
    #[inline(always)]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let amount_read = self.current_buffer.read(buf)?;
        if amount_read > 0 {
            return Ok(amount_read);
        }

        self.read_slow(buf)
    }
}

impl MultiplexReader {
    /// slow path for reads, try to get a new buffer or
    /// return zero if at the end of the stream
    #[cold]
    #[inline(never)]
    fn read_slow(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while !self.end_of_file {
            let amount_read = self.current_buffer.read(buf)?;
            if amount_read > 0 {
                return Ok(amount_read);
            }

            match self.receiver.recv() {
                Ok(r) => match r {
                    Message::Eof(_tid) => {
                        self.end_of_file = true;
                    }
                    Message::WriteBlock(tid, block) => {
                        debug_assert_eq!(
                            tid, self.partition_id,
                            "incoming thread must be equal to processing thread"
                        );
                        self.current_buffer = Cursor::new(block);
                    }
                },
                Err(e) => {
                    return std::io::Result::Err(std::io::Error::new(std::io::ErrorKind::Other, e));
                }
            }
        }

        // nothing if we reached the end of file
        return Ok(0);
    }
}

/// Reads data in multiplexed format and sends it to the appropriate processor, each
/// running on its own thread. The processor function is called with the partition_id and
/// a blocking reader that it can use to read its own data.
///
/// Once the multiplexed data is finished reading, we break the channel to the worker threads
/// causing processor that is trying to read from the channel to error out and exit. After all
/// the readers have exited, we collect the results/errors from all the processors and return a vector
/// of the results back to the caller.
pub struct MultiplexReaderState<RESULT> {
    sender_channels: Vec<Sender<Message>>,
    receiver_channels: Vec<Receiver<MultiplexReadResult<RESULT>>>,
    retention_bytes: usize,
    current_state: State,
    single_thread_work: Option<VecDeque<Box<dyn FnOnce() + Send>>>,
    merged_metrics: Metrics,
}

enum State {
    StartBlock,
    U16Length(u8),
    Block(u8, usize),
}

pub enum MultiplexReadResult<RESULT> {
    Result(RESULT),
    Error(LeptonError),
    Complete(Metrics),
}

/// Given a number of threads, this function will create a multiplexed reader state that
/// can be used to process incoming multiplexed data. The processor function is called
/// on each thread with the partition_id and a blocking reader that it can use to read its own data.
///
/// Each processor is also given a sender channel that it can use to send back results or errors.
/// Partial results can be sent back by sending multiple results before the end of file is reached.
///
/// The state object returned can be used to process incoming data and retrieve results/errors
/// from the threads.
pub fn multiplex_read<FN, RESULT>(
    num_partitions: usize,
    max_processor_threads: usize,
    thread_pool: &dyn LeptonThreadPool,
    retention_bytes: usize,
    processor: FN,
) -> MultiplexReaderState<RESULT>
where
    FN: Fn(usize, &mut MultiplexReader, &Sender<MultiplexReadResult<RESULT>>) -> Result<()>
        + Send
        + Sync
        + 'static,
    RESULT: Send + 'static,
{
    let arc_processor = Arc::new(Box::new(processor));

    let mut channel_to_sender = Vec::new();

    // collect the worker threads in a queue so we can spawn them
    let mut work = VecDeque::new();
    let mut result_receiver = Vec::new();

    for partition_id in 0..num_partitions {
        let (tx, rx) = channel::<Message>();
        channel_to_sender.push(tx);

        let cloned_processor = arc_processor.clone();

        let (result_tx, result_rx) = channel::<MultiplexReadResult<RESULT>>();
        result_receiver.push(result_rx);

        let f: Box<dyn FnOnce() + Send> = Box::new(move || {
            // get the appropriate receiver so we can read out data from it
            let mut proc_reader = MultiplexReader {
                partition_id,
                current_buffer: Cursor::new(Vec::new()),
                receiver: rx,
                end_of_file: false,
            };

            if let Err(e) =
                catch_unwind_result(|| cloned_processor(partition_id, &mut proc_reader, &result_tx))
            {
                let _ = result_tx.send(MultiplexReadResult::Error(e));
            }
        });

        work.push_back(f);
    }

    let single_thread_work = if thread_pool.max_parallelism() > 1 {
        spawn_processor_threads(thread_pool, max_processor_threads, work);
        None
    } else {
        Some(work)
    };

    MultiplexReaderState {
        sender_channels: channel_to_sender,
        receiver_channels: result_receiver,
        current_state: State::StartBlock,
        retention_bytes,
        single_thread_work,
        merged_metrics: Metrics::default(),
    }
}

/// spawns the processor threads to handle the work items in the queue. There may be fewer workers
/// than work items.
fn spawn_processor_threads(
    thread_pool: &dyn LeptonThreadPool,
    max_processor_threads: usize,
    work: VecDeque<Box<dyn FnOnce() + Send>>,
) {
    let work_threads = work.len().min(max_processor_threads);
    let shared_queue = Arc::new(Mutex::new(work));

    // spawn the worker threads to process all the items
    // (there may be less processor threads than the number of threads in the image)
    for _i in 0..work_threads {
        let q = shared_queue.clone();

        thread_pool.run(Box::new(move || {
            loop {
                // do this to make sure the lock gets
                let w = q.lock().unwrap().pop_front();

                if let Some(f) = w {
                    f();
                } else {
                    break;
                }
            }
        }));
    }
}

impl<RESULT> MultiplexReaderState<RESULT> {
    /// process as much incoming data as we can and send it to the appropriate thread
    pub fn process_buffer(&mut self, source: &mut PartialBuffer<'_>) -> Result<()> {
        while source.continue_processing() {
            match self.current_state {
                State::StartBlock => {
                    if let Some(a) = source.take_n::<1>(self.retention_bytes) {
                        let thread_marker = a[0];

                        let partition_id = thread_marker & 0xf;

                        if usize::from(partition_id) >= self.sender_channels.len() {
                            return err_exit_code(
                                ExitCode::BadLeptonFile,
                                format!("invalid partition_id {0}", partition_id),
                            );
                        }

                        if thread_marker < 16 {
                            self.current_state = State::U16Length(partition_id);
                        } else {
                            let flags = (thread_marker >> 4) & 3;
                            self.current_state = State::Block(partition_id, 1024 << (2 * flags));
                        }
                    } else {
                        break;
                    }
                }
                State::U16Length(thread_marker) => {
                    if let Some(a) = source.take_n::<2>(self.retention_bytes) {
                        let b0 = usize::from(a[0]);
                        let b1 = usize::from(a[1]);

                        self.current_state = State::Block(thread_marker, (b1 << 8) + b0 + 1);
                    } else {
                        break;
                    }
                }
                State::Block(partition_id, data_length) => {
                    if let Some(a) = source.take(data_length, self.retention_bytes) {
                        // ignore if we get error sending because channel died since we will collect
                        // the error later. We don't want to interrupt the other threads that are processing
                        // so we only get the error from the thread that actually errored out.
                        let tid = usize::from(partition_id);
                        let _ = self.sender_channels[tid].send(Message::WriteBlock(tid, a));
                        self.current_state = State::StartBlock;
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// retrieves the next available result from the threads. If complete is true, this function
    /// will block until all threads are complete and return the first result or error it finds.
    /// If complete is false, this function will return immediately if no results are available.
    pub fn retrieve_result(&mut self, complete: bool) -> Result<Option<RESULT>> {
        if let Some(value) =
            Self::try_get_result(&mut self.receiver_channels, &mut self.merged_metrics)?
        {
            return Ok(Some(value));
        }

        if complete {
            // if we are complete, send eof to all threads
            for partition_id in 0..self.sender_channels.len() {
                // send eof to all threads (ignore results since they might be dead already)
                let _ = self.sender_channels[partition_id].send(Message::Eof(partition_id));
            }
            self.sender_channels.clear();

            // if we are running single threaded, now do all the work since we've buffered up everything
            // and broken the sender channels, so there's no danger of deadlock
            if let Some(single_thread_work) = &mut self.single_thread_work {
                while let Some(f) = single_thread_work.pop_front() {
                    f();

                    if let Some(value) =
                        Self::try_get_result(&mut self.receiver_channels, &mut self.merged_metrics)?
                    {
                        return Ok(Some(value));
                    }
                }
            }

            // if we are complete, then walk through all the channels to get the first result by blocking
            while let Some(r) = self.receiver_channels.get_mut(0) {
                match r.recv() {
                    Ok(v) => match v {
                        MultiplexReadResult::Result(v) => return Ok(Some(v)),
                        MultiplexReadResult::Error(e) => return Err(e),
                        MultiplexReadResult::Complete(m) => {
                            // finished, so remove it and try the next one
                            self.merged_metrics.merge_from(m);
                            self.receiver_channels.remove(0);
                        }
                    },
                    Err(e) => {
                        // channel is closed unexpectedly, clear out all channels and return error
                        self.receiver_channels.clear();
                        return Err(e.into());
                    }
                }
            }
        }
        // nothing left to read
        Ok(None)
    }

    /// tries to get a result from the receiver channels without blocking
    fn try_get_result(
        receiver_channels: &mut Vec<Receiver<MultiplexReadResult<RESULT>>>,
        metrics: &mut Metrics,
    ) -> Result<Option<RESULT>> {
        // if we aren't complete, use non-blocking to try to get some results
        // from the first thread
        while let Some(r) = receiver_channels.get_mut(0) {
            match r.try_recv() {
                Ok(v) => match v {
                    MultiplexReadResult::Result(v) => return Ok(Some(v)),
                    MultiplexReadResult::Error(e) => return Err(e),
                    MultiplexReadResult::Complete(m) => {
                        // finished, so remove it and try the next one
                        metrics.merge_from(m);
                        receiver_channels.remove(0);
                    }
                },
                Err(TryRecvError::Disconnected) => {
                    // finished, so remove it and try the next one
                    return Err(LeptonError::new(
                        ExitCode::AssertionFailure,
                        "multiplexed reader channel disconnected unexpectedly",
                    ));
                }
                Err(TryRecvError::Empty) => {
                    // no result yet, exit loop without result
                    break;
                }
            }
        }
        Ok(None)
    }

    /// takes the merged metrics from all the threads
    pub fn take_metrics(&mut self) -> Metrics {
        std::mem::take(&mut self.merged_metrics)
    }
}
