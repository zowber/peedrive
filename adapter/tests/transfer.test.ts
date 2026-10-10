import { expect, test } from 'bun:test';
import { Throttle } from '../src/throttle';

const bytes = (n: number) => new Uint8Array(n).map((_, i) => i % 251);
async function collect(stream: ReadableStream<Uint8Array>) {
    const chunks: Uint8Array[] = []; const reader = stream.getReader();
    for (let r = await reader.read(); !r.done; r = await reader.read()) chunks.push(r.value);
    return chunks;
}

test('unlimited transfers pass through without waiting', async () => {
    const start = Date.now();
    const chunks = await collect(new Throttle().readable(new Blob([bytes(1_000_000)]).stream()));
    expect(Buffer.concat(chunks)).toEqual(Buffer.from(bytes(1_000_000)));
    expect(Math.max(...chunks.map(c => c.length))).toBeLessThanOrEqual(64 * 1024);
    expect(Date.now() - start).toBeLessThan(200);
});

test('concurrent transfers share one limit', async () => {
    const throttle = new Throttle(); throttle.rate = 400_000;
    const start = Date.now();
    await Promise.all([0, 1].map(() => collect(throttle.readable(new Blob([bytes(100_000)]).stream()))));
    // 200 kB at 400 kB/s; each alone would take 250 ms.
    expect(Date.now() - start).toBeGreaterThanOrEqual(450);
});

test('throttled writes keep every byte and close the destination', async () => {
    const throttle = new Throttle(); throttle.rate = 10_000_000;
    const written: Uint8Array[] = []; let closed = false;
    const sink = new WritableStream<Uint8Array>({ write: c => { written.push(c.slice()); }, close: () => { closed = true; } });
    const stream = throttle.writable(sink.getWriter());
    const writer = stream.getWriter(); await writer.write(bytes(300_000)); writer.releaseLock();
    await stream.close();
    expect(Buffer.concat(written)).toEqual(Buffer.from(bytes(300_000))); expect(closed).toBe(true);
});
