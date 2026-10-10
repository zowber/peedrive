import { describe, expect, test } from 'bun:test';
import { relativePath, requireExpected, type Entry } from '../src/protocol';
import { classify } from '../src/errors';

describe('local paths supplied by remote names', () => {
    for (const path of ['', '../escape', '/absolute', 'a/../b', 'a//b', 'a/./b', 'a/\0b', '.peedrive-tmp/a']) {
        test(`rejects ${JSON.stringify(path)}`, () => expect(() => relativePath(path)).toThrow());
    }
    test('allows ordinary nested unicode names', () => expect(relativePath('Projects/Café notes.txt')).toBe('Projects/Café notes.txt'));
});
test('remote preconditions reject revision and identity changes', () => {
    const e: Entry = { kind: 'file', uid: 'n1', version: 'r1', size: 5, hash: null };
    expect(() => requireExpected({ ...e, version: 'r2' }, e)).toThrow();
    expect(() => requireExpected({ ...e, uid: 'n2' }, e)).toThrow();
    expect(() => requireExpected(null, e)).toThrow();
    expect(() => requireExpected(e, null)).toThrow();
    expect(() => requireExpected(e, e)).not.toThrow();
});

describe('error classification', () => {
    const coded = (code: string) => Object.assign(new Error('request failed'), { code });
    test('network failures retry', () => {
        for (const e of [new Error('getaddrinfo ETIMEOUT drive-api.proton.me'), coded('ECONNRESET'), coded('ConnectionRefused'),
            Object.assign(new Error('Server error'), { name: 'ServerError', statusCode: 503 }), Object.assign(new Error('x'), { name: 'RateLimitedError' }),
            new Error('wrapped', { cause: coded('EAI_AGAIN') })]) expect(classify(e)).toBe('network');
    });
    test('missing sessions wait for sign-in', () => {
        expect(classify(new Error('Sign in to Proton Drive first.'))).toBe('auth');
        expect(classify(Object.assign(new Error('Unauthorized'), { statusCode: 401 }))).toBe('auth');
    });
    test('integrity and conflict failures stop for review', () => {
        for (const e of [new Error('Download signature verification failed'), new Error('Remote item changed since planning. Review and retry.'),
            Object.assign(new Error('Bad request'), { statusCode: 422 }), 'not an error']) expect(classify(e)).toBe('fatal');
    });
});

test('the app version follows Proton\'s external-drive pattern', async () => {
    const { APP_VERSION } = await import('../src/client');
    expect(APP_VERSION).toMatch(/^external-drive-[a-z_]+@\d+\.\d+\.\d+(-(stable|beta|alpha))?(\+[0-9A-Za-z.-]+)?$/);
});
