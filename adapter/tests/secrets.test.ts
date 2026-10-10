import { expect, test } from 'bun:test';
import { SecretStore } from '../src/client';

// An in-memory stand-in for GNOME Keyring; these tests never touch the real one.
function keyring(failWrites = false) {
    const items = new Map<string, string>();
    const id = (k: { service: string; name: string }) => `${k.service}/${k.name}`;
    return {
        items,
        secrets: {
            async get(k: { service: string; name: string }) { return items.get(id(k)) ?? null; },
            async set(k: { service: string; name: string; value: string }) { if (!failWrites) items.set(id(k), k.value); },
            async delete(k: { service: string; name: string }) { return items.delete(id(k)); },
        } as any,
    };
}
const session = JSON.stringify({ cachePassword: 'cache', userKeyPassword: 'key', session: { uid: 'u', accessToken: 'a', refreshToken: 'r' } });
const OLD = 'io.github.andybright.Peedrive/account-session-v1';
const NEW = 'io.github.zowber.Peedrive/account-session-v1';

test('a session saved under the old app ID moves to the new one', async () => {
    const k = keyring(); k.items.set(OLD, session);
    expect((await new SecretStore(k.secrets).load())?.cachePassword).toBe('cache');
    expect(k.items.get(NEW)).toBe(session);
    expect(k.items.has(OLD)).toBe(false);
});
test('the old entry stays if the new one could not be written', async () => {
    const k = keyring(true); k.items.set(OLD, session);
    expect((await new SecretStore(k.secrets).load())?.session.uid).toBe('u');
    expect(k.items.has(OLD)).toBe(true);
});
test('the new entry wins and sign-out clears both', async () => {
    const k = keyring(); k.items.set(NEW, session); k.items.set(OLD, 'stale');
    const store = new SecretStore(k.secrets);
    expect((await store.load())?.userKeyPassword).toBe('key');
    await store.remove();
    expect(k.items.size).toBe(0);
});
