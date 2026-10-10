/** Larger chunks are split so a low limit still reports steady progress. */
const SLICE = 64 * 1024;

/**
 * A rate limit shared by every transfer in one direction. Each slice reserves the next free
 * slot of time, so concurrent transfers split the limit between them.
 */
export class Throttle {
    private free = 0;
    /** Bytes per second; 0 or undefined is unlimited. */
    rate?: number;
    async take(bytes: number) {
        if (!this.rate) return;
        const now = Date.now();
        this.free = Math.max(now, this.free) + bytes / this.rate * 1000;
        if (this.free > now) await Bun.sleep(this.free - now);
    }
    async *slices(chunk: Uint8Array) {
        for (let i = 0; i < chunk.length; i += SLICE) {
            const slice = chunk.subarray(i, i + SLICE);
            await this.take(slice.length);
            yield slice;
        }
    }
    readable(source: ReadableStream<Uint8Array>): ReadableStream<Uint8Array> {
        const reader = source.getReader();
        let pending: AsyncGenerator<Uint8Array> | undefined;
        return new ReadableStream({
            pull: async controller => {
                for (;;) {
                    const next = await pending?.next();
                    if (next && !next.done) return controller.enqueue(next.value);
                    const read = await reader.read();
                    if (read.done) return controller.close();
                    pending = this.slices(read.value);
                }
            },
            cancel: reason => reader.cancel(reason),
        });
    }
    writable(sink: WritableStreamDefaultWriter<Uint8Array>): WritableStream<Uint8Array> {
        return new WritableStream({
            write: async chunk => { for await (const slice of this.slices(chunk)) await sink.write(slice); },
            close: () => sink.close(),
            abort: reason => sink.abort(reason),
        });
    }
}
