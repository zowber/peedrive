export const SIGNED_OUT = 'Sign in to Proton Drive first.';
export type ErrorKind = 'network' | 'auth' | 'fatal';

const NETWORK_NAMES = new Set(['ConnectionError', 'RateLimitedError', 'TimeoutError']);
const NETWORK_CODES = new Set(['ECONNRESET', 'ECONNREFUSED', 'ECONNABORTED', 'ETIMEDOUT', 'ENOTFOUND', 'EAI_AGAIN',
    'ENETUNREACH', 'ENETDOWN', 'EHOSTUNREACH', 'EPIPE', 'ConnectionRefused', 'ConnectionClosed', 'FailedToOpenSocket']);
const NETWORK_MESSAGE = /getaddrinfo|fetch failed|network|socket|timed? ?out|ETIMEOUT|ECONN|unable to connect/i;

/** Transient failures retry automatically; only `fatal` stops a mapping for review. */
export function classify(error: unknown, depth = 0): ErrorKind {
    if (!(error instanceof Error) || depth > 4) return 'fatal';
    const e = error as Error & { code?: unknown; statusCode?: number; response?: { status?: number } };
    if (e.message === SIGNED_OUT) return 'auth';
    const status = e.statusCode ?? e.response?.status;
    if (status === 401) return 'auth';
    if (status !== undefined && (status >= 500 || status === 429 || status === 408)) return 'network';
    if (NETWORK_NAMES.has(e.name) || (typeof e.code === 'string' && NETWORK_CODES.has(e.code))) return 'network';
    if (NETWORK_MESSAGE.test(e.message)) return 'network';
    return e.cause === undefined ? 'fatal' : classify(e.cause, depth + 1);
}
