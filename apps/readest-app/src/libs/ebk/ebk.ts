// EBK books (https://github.com/Sraw/ebk): an EPUB's files, compressed better (text with brotli in blocks, JPEG
// with Lepton). They are read by ebk.wasm in a worker (src/workers/ebk.worker.ts) and rendered as the EPUB they
// hold. The wasm module is built from packages/ebk (packages/ebk/build.sh).

const MAGIC = [0x89, 0x45, 0x42, 0x4b, 0x0d, 0x0a, 0x1a, 0x0a];

export const isEBK = async (file: Blob) => {
  const head = new Uint8Array(await file.slice(0, 8).arrayBuffer());
  return head.length === 8 && MAGIC.every((b, i) => head[i] === b);
};

export interface EbkMember {
  path: string;
  // storage mode: 0 and 1 are in the text stream
  mode: number;
  size: number;
  storedSize: number;
  readable: boolean;
}

export class EbkError extends Error {
  // invalid, corrupt, too-large, unsupported, no-such-member, io, trap, timeout, closed
  status: string;
  constructor({ status, message }: { status: string; message: string }) {
    super(message);
    this.name = 'EbkError';
    this.status = status;
  }
}

interface Options {
  // the longest member (bytes) and the largest recompressed JPEG (pixels) that will be read - the reader allocates
  // up to 6 bytes per pixel
  memberLimit: number;
  pixelLimit: number;
  // how long one request may take once it has started (milliseconds)
  timeout: number;
  // how much of the text is decoded ahead of need after the file is opened, in the background (bytes; 0: none)
  preload: number;
  // after how long without requests the worker is stopped, with its memory (milliseconds); the next request starts
  // a new one. Readest keeps a closed book's document for reopening it, so this is what frees a book not read.
  idle: number;
}

type Reply = { ok: true; [key: string]: unknown };
type Request = {
  type: string;
  args: object;
  resolve: (reply: Reply) => void;
  reject: (e: unknown) => void;
};

let wasmModule: Promise<WebAssembly.Module> | undefined;
const compiled = () =>
  (wasmModule ??= fetch(new URL('./ebk.wasm', import.meta.url))
    .then((response) => response.arrayBuffer())
    .then((bytes) => WebAssembly.compile(bytes)));

export class EbkFile {
  #file!: Blob;
  #options!: Options;
  // the worker that has the file open, if there is one; it is replaced after a trap or a timeout
  #worker: Worker | null = null;
  #starting: Promise<{ members: EbkMember[] }> | null = null;
  // requests wait here and go to the worker one at a time
  #queue: Request[] = [];
  #pumping = false;
  #abort: (() => void) | null = null;
  #closed = false;
  #idleTimer: ReturnType<typeof setTimeout> | undefined;
  // members that made the module trap: they are not tried again, each try would cost a new worker
  #trapped = new Set<number>();
  #byPath = new Map<string, number>();
  members: EbkMember[] = [];

  /** `file` must hold its bytes (a plain Blob or File): the worker reads it with FileReaderSync. */
  static async open(file: Blob, options: Partial<Options> = {}) {
    const ebk = new EbkFile();
    ebk.#file = file;
    ebk.#options = {
      memberLimit: 256 * 2 ** 20,
      pixelLimit: 2 ** 25,
      timeout: 60000,
      preload: 64 * 2 ** 20,
      idle: 60000,
      ...options,
    };
    const { members } = await ebk.#start();
    ebk.members = members;
    members.forEach((m, index) => ebk.#byPath.set(m.path, index));
    // the whole text, decoded in the background, so that turning to any chapter later waits for nothing
    if (ebk.#options.preload > 0) ebk.#preload(0);
    return ebk;
  }

  // One step of decoding ahead, taken only when nothing the page asked for is waiting, so the first page is not
  // held up by the rest of the book. It stops at the end, at the limit, when the file is closed, and when a step
  // fails (a damaged block that stopped the module: the next worker decodes block by block).
  async #preload(from: number) {
    const worker = this.#worker;
    while (!this.#closed && from < this.members.length && this.#worker === worker) {
      if (this.#queue.length || this.#pumping) {
        await new Promise((resolve) => setTimeout(resolve, 20));
        continue;
      }
      try {
        // small steps: a chapter the page asks for waits at most for one of them
        const reply = await this.#request('preload', {
          from,
          step: 256 * 2 ** 10,
          limit: this.#options.preload,
        });
        from = reply['next'] as number;
      } catch {
        break;
      }
    }
  }

  // Starts a worker and opens the file in it. However many callers ask at once, one worker is started.
  #start() {
    return (this.#starting ??= (async () => {
      try {
        const wasm = await compiled();
        const worker = new Worker(new URL('../../workers/ebk.worker.ts', import.meta.url), {
          type: 'module',
        });
        const { memberLimit, pixelLimit } = this.#options;
        const reply = await this.#exchange(worker, 'open', {
          file: this.#file,
          wasm,
          memberLimit,
          pixelLimit,
        });
        if (this.#closed) worker.terminate();
        else this.#worker = worker;
        return reply as unknown as { members: EbkMember[] };
      } finally {
        this.#starting = null;
      }
    })());
  }

  // One request to a worker and its reply. The worker is thrown away when the module trapped (its memory can no
  // longer be trusted), when it does not answer in time, and when it failed to open the file.
  #exchange(worker: Worker, type: string, args: object) {
    return new Promise<Reply>((resolve, reject) => {
      const settle = (done: () => void, keep: boolean) => {
        clearTimeout(timer);
        worker.onmessage = worker.onerror = null;
        this.#abort = null;
        if (!keep) {
          worker.terminate();
          if (this.#worker === worker) this.#worker = null;
        }
        done();
      };
      const fail = (status: string, message: string) =>
        settle(() => reject(new EbkError({ status, message })), false);
      const timer = setTimeout(
        () => fail('timeout', 'the reader did not answer'),
        this.#options.timeout,
      );
      this.#abort = () => fail('closed', 'the file was closed');
      worker.onmessage = ({ data }) => {
        if (data.ok) settle(() => resolve(data), true);
        else settle(() => reject(new EbkError(data)), !data.fatal && type !== 'open');
      };
      worker.onerror = (e) => fail('trap', e.message ?? 'the reader stopped');
      worker.postMessage({ type, ...args });
    });
  }

  // Requests run one at a time. The module does one thing at a time anyway; this way the time limit measures a
  // request and not its wait behind others, and a request that fails takes no other request with it.
  #request(type: string, args: object) {
    clearTimeout(this.#idleTimer);
    return new Promise<Reply>((resolve, reject) => {
      this.#queue.push({ type, args, resolve, reject });
      this.#pump();
    });
  }

  async #pump() {
    if (this.#pumping) return;
    this.#pumping = true;
    while (this.#queue.length) {
      const { type, args, resolve, reject } = this.#queue.shift()!;
      try {
        if (this.#closed) throw new EbkError({ status: 'closed', message: 'the file was closed' });
        if (!this.#worker) await this.#start();
        if (this.#closed) throw new EbkError({ status: 'closed', message: 'the file was closed' });
        resolve(await this.#exchange(this.#worker!, type, args));
      } catch (e) {
        reject(e);
      }
    }
    this.#pumping = false;
    if (!this.#closed) this.#idleTimer = setTimeout(() => this.#rest(), this.#options.idle);
  }

  #rest() {
    if (this.#queue.length || this.#pumping) return;
    this.#worker?.terminate();
    this.#worker = null;
  }

  size(path: string) {
    const index = this.#byPath.get(path);
    return index === undefined ? 0 : this.members[index]!.size;
  }

  /** The bytes of a member, checked against its length and checksum; null when there is no such member. */
  async read(path: string) {
    const index = this.#byPath.get(path);
    if (index === undefined) return null;
    if (this.#trapped.has(index)) {
      throw new EbkError({ status: 'trap', message: 'this member stopped the reader before' });
    }
    try {
      return new Uint8Array((await this.#request('read', { index }))['data'] as ArrayBuffer);
    } catch (e) {
      if (e instanceof EbkError && e.status === 'trap') this.#trapped.add(index);
      throw e;
    }
  }

  close() {
    this.#closed = true;
    clearTimeout(this.#idleTimer);
    this.#abort?.();
    this.#worker?.terminate();
    this.#worker = null;
  }

  /** What foliate-js needs to read an EPUB. */
  get loader() {
    const decoder = new TextDecoder();
    return {
      entries: this.members.map((m) => ({ filename: m.path })),
      loadText: async (name: string) => {
        const data = await this.read(name);
        return data ? decoder.decode(data) : null;
      },
      loadBlob: async (name: string, type?: string) => {
        const data = await this.read(name);
        return data ? new Blob([data], { type }) : null;
      },
      getSize: (name: string) => this.size(name),
      // foliate-js asks for it to identify Adobe's font obfuscation; such books are rare
      sha1: undefined,
    };
  }
}
