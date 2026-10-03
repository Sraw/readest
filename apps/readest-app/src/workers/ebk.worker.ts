// Runs the EBK reader (ebk.wasm) for one file, for src/libs/ebk/ebk.ts. The reader asks for the byte ranges it
// needs and they are read from the Blob with FileReaderSync, which exists only in workers.
//
// The page sends one request and waits for its reply before it sends the next.
// Requests: { type: 'open', file, wasm, memberLimit, pixelLimit }, { type: 'preload', from, step, limit },
// { type: 'read', index }.
// Replies: { ok: true, ... } or { ok: false, status, message }; `fatal: true` when the module trapped and this
// worker must be thrown away.

interface Exports {
  memory: WebAssembly.Memory;
  ebk_open(size: number, memberLimit: number, pixelLimit: number): number;
  ebk_members(): number;
  ebk_read(index: number): number;
  ebk_retry(): number;
  ebk_result_ptr(): number;
  ebk_result_len(): number;
}

interface Member {
  path: string;
  mode: number;
  size: number;
  storedSize: number;
  readable: boolean;
}

const workerContext = self as unknown as DedicatedWorkerGlobalScope;
const STATUS = [
  'ok',
  'invalid',
  'corrupt',
  'too-large',
  'unsupported',
  'no-such-member',
  'io',
  'not-open',
];

let exports: Exports | undefined;
let file: Blob;
let members: Member[] = [];
// what 'preload' decoded: member index -> bytes
const preloaded = new Map<number, ArrayBuffer>();
let preloadedBytes = 0;
const sync = new FileReaderSync();

const imports = {
  env: {
    host_read(offset: number, ptr: number, len: number) {
      try {
        const bytes = sync.readAsArrayBuffer(file.slice(offset, offset + len));
        if (bytes.byteLength !== len) return 1;
        // the view is made now: the module's memory may have grown, and moved, since the last call
        new Uint8Array(exports!.memory.buffer, ptr, len).set(new Uint8Array(bytes));
        return 0;
      } catch {
        return 1;
      }
    },
  },
};

// a copy of the result: the module's memory is reused by the next call
const result = () =>
  exports!.memory.buffer.slice(
    exports!.ebk_result_ptr(),
    exports!.ebk_result_ptr() + exports!.ebk_result_len(),
  );
const failure = (status: number) => ({
  ok: false,
  status: STATUS[status] ?? 'unknown',
  message: new TextDecoder().decode(result()),
});

type Reply = { ok: boolean; transfer?: Transferable[]; [key: string]: unknown };

const handlers: Record<string, (args: Record<string, unknown>) => Promise<Reply> | Reply> = {
  async open(args) {
    file = args['file'] as Blob;
    // `wasm` is a compiled module, so this gives the instance itself
    const instance = await WebAssembly.instantiate(args['wasm'] as WebAssembly.Module, imports);
    exports = instance.exports as unknown as Exports;
    const status = exports.ebk_open(
      file.size,
      args['memberLimit'] as number,
      args['pixelLimit'] as number,
    );
    if (status) return failure(status);
    exports.ebk_members();
    members = new TextDecoder()
      .decode(result())
      .split('\n')
      .filter((line) => line)
      .map((line) => {
        const [mode, size, storedSize, readable, ...path] = line.split('\t');
        return {
          path: path.join('\t'),
          mode: Number(mode),
          size: Number(size),
          storedSize: Number(storedSize),
          readable: readable === '1',
        };
      });
    return { ok: true, members };
  },
  // Decodes the text of the book ahead of need, in steps the page asks for one at a time between its own requests:
  // members of the text stream from number `from` on, in order (each text block is decoded once), about `step`
  // bytes, and no more than `limit` bytes kept in all. A member that cannot be read is left for 'read' to report.
  preload(args) {
    const step = args['step'] as number;
    const limit = args['limit'] as number;
    let index = args['from'] as number;
    for (let done = 0; index < members.length && done < step; index++) {
      const m = members[index]!;
      // storage modes 0 and 1: the members in the text stream
      if (m.mode > 1 || !m.readable || preloadedBytes + m.size > limit) continue;
      const status = exports!.ebk_read(index);
      // 3: too large, or no memory for the block: it is left to be tried when it is read, and nothing more is
      // decoded ahead
      if (status === 3) {
        exports!.ebk_retry();
        return { ok: true, next: members.length };
      }
      if (status) continue;
      preloaded.set(index, result());
      preloadedBytes += m.size;
      done += m.size;
    }
    return { ok: true, next: index };
  },
  read(args) {
    const index = args['index'] as number;
    // a copy: what is sent is gone from here
    const kept = preloaded.get(index);
    if (kept) {
      const data = kept.slice(0);
      return { ok: true, data, transfer: [data] };
    }
    const status = exports!.ebk_read(index);
    if (status) return failure(status);
    const data = result();
    return { ok: true, data, transfer: [data] };
  },
};

workerContext.onmessage = async ({ data: { type, ...args } }: MessageEvent) => {
  if (type !== 'open' && !exports) {
    workerContext.postMessage({
      ok: false,
      status: 'not-open',
      message: 'no file is open in this worker',
    });
    return;
  }
  try {
    const { transfer, ...reply } = await handlers[type]!(args);
    workerContext.postMessage(reply, transfer ?? []);
  } catch (e) {
    // a trap inside the module (a panic, or memory that could not be had): its state is gone
    workerContext.postMessage({
      ok: false,
      fatal: true,
      status: 'trap',
      message: String((e as Error)?.message ?? e),
    });
  }
};
