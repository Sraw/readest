// An EBK file holds an EPUB's files and is rendered as that EPUB: opened by DocumentLoader it must give the same
// book as the EPUB it was made from. The fixtures were made with `ebk convert` (https://github.com/Sraw/ebk):
// sample-alice.ebk from sample-alice.epub (text and PNG pictures), sample-jpeg.ebk from sample-jpeg.epub (a JPEG
// picture, which EBK recompresses with Lepton).
import { describe, it, expect } from 'vitest';
import { DocumentLoader } from '@/libs/document';
import type { BookDoc } from '@/libs/document';
import { EbkFile } from '@/libs/ebk/ebk';

const fixture = async (name: string) => {
  const response = await fetch(new URL(`../fixtures/data/${name}`, import.meta.url).href);
  return new File([await response.arrayBuffer()], name);
};

// Readest's NativeFile on Tauri is a File whose bytes are read on demand
class OnDemandFile extends File {}

const sectionTexts = async (book: BookDoc) => {
  const sections = book.sections as unknown as { id: string; loadText?: () => Promise<string> }[];
  return Promise.all(sections.map(async (s) => [s.id, await s.loadText?.()]));
};

const bytes = async (blob: Blob | null) => (blob ? new Uint8Array(await blob.arrayBuffer()) : null);

describe.each(['sample-alice', 'sample-jpeg'])('DocumentLoader with %s.ebk', (name) => {
  it('opens it as the EPUB it was made from', async () => {
    const epub = (await new DocumentLoader(await fixture(`${name}.epub`)).open()).book;
    const { book, format } = await new DocumentLoader(await fixture(`${name}.ebk`)).open();
    try {
      expect(format).toBe('EBK');
      expect(book.metadata.title).toEqual(epub.metadata.title);
      expect(book.toc).toEqual(epub.toc);
      const texts = await sectionTexts(book);
      expect(texts.every(([, text]) => typeof text === 'string' && text.length > 0)).toBe(true);
      expect(texts).toEqual(await sectionTexts(epub));
      const cover = await bytes(await book.getCover());
      expect(cover).not.toBeNull();
      expect(cover).toEqual(await bytes(await epub.getCover()));
    } finally {
      await book.destroy?.();
    }
  });

  it('opens it from a file whose bytes are read on demand', async () => {
    const file = await fixture(`${name}.ebk`);
    const { book, format } = await new DocumentLoader(new OnDemandFile([file], file.name)).open();
    try {
      expect(format).toBe('EBK');
      expect((await sectionTexts(book)).length).toBeGreaterThan(0);
    } finally {
      await book.destroy?.();
    }
  });
});

describe('DocumentLoader with a damaged EBK file', () => {
  it('refuses it', async () => {
    const file = await fixture('sample-alice.ebk');
    const damaged = new Uint8Array(await file.arrayBuffer()).slice(0, 4096);
    await expect(new DocumentLoader(new File([damaged], 'damaged.ebk')).open()).rejects.toThrow();
  });
});

describe('EbkFile', () => {
  it('stops its worker when idle and starts another for the next read', async () => {
    const ebk = await EbkFile.open(await fixture('sample-jpeg.ebk'), { idle: 50, preload: 0 });
    try {
      const first = await ebk.read('OEBPS/cover.jpg');
      await new Promise((resolve) => setTimeout(resolve, 200));
      expect(await ebk.read('OEBPS/cover.jpg')).toEqual(first);
    } finally {
      ebk.close();
    }
  });

  it('reads nothing once the book is destroyed', async () => {
    const { book } = await new DocumentLoader(await fixture('sample-alice.ebk')).open();
    await book.destroy?.();
    await expect(book.loadText!('OPS/fb.opf')).rejects.toThrow('closed');
  });
});
