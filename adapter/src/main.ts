import { createInterface } from 'node:readline';
import path from 'node:path';
import { APP_VERSION, createClient } from './client';
import { Drive } from './drive';
import { requireString } from './protocol';
import { classify } from './errors';

process.umask(0o077);
const send = (value: unknown) => process.stdout.write(JSON.stringify(value) + '\n');
let client: Awaited<ReturnType<typeof createClient>> | undefined;
let drive: Drive | undefined;
let login: Promise<unknown> | undefined;
let loginUrl: string | undefined;
let authError: string | undefined;
let authController: AbortController | undefined;
const lastProgress = new Map<unknown, number>();
let quota: { used: number; max: number } | undefined;
let quotaAt = 0;
let starting: Promise<Awaited<ReturnType<typeof createClient>>> | undefined;
/** Concurrent first requests share one client. */
function ready() {
    starting ??= (async () => {
        const dir = requireString(process.env.PEEDRIVE_DATA_DIR);
        if (!path.isAbsolute(dir)) throw new Error('Data directory must be absolute');
        client = await createClient(path.join(dir, 'sdk'));
        drive = new Drive(client);
        return client;
    })().catch(error => { starting = undefined; throw error; });
    return starting;
}
/** Progress events name the request they belong to, so concurrent transfers stay apart. */
function transfer(id: unknown, p: Record<string, any>) {
    const limit = Number.isSafeInteger(p.limit) && p.limit > 0 ? p.limit : undefined;
    return { limit, progress: (bytes: number) => {
        if (Date.now() - (lastProgress.get(id) ?? 0) > 200) { send({ event: 'progress', id, bytes }); lastProgress.set(id, Date.now()); }
    } };
}
async function dispatch(method: string, p: Record<string, any>, id: unknown) {
    if (method === 'hello') return { protocol: 1, version: APP_VERSION, sdk_revision: '8ffae007b0fbb8bcef53514719d6f093bb32dcf9' };
    const c = await ready();
    if (method === 'auth.status') {
        const signedIn = c.auth.isLoggedIn();
        let email: string | null = null;
        let error = authError ?? null;
        if (signedIn) {
            try { email = (await c.addresses.getOwnPrimaryAddress()).email; }
            catch { error = 'Connected, but account details could not be loaded. Retrying…'; }
        }
        if (signedIn && Date.now() - quotaAt > 300_000) {
            quotaAt = Date.now();
            try { const user = (await c.accountApi.users()).User; if (user) quota = { used: user.UsedSpace, max: user.MaxSpace }; }
            catch { /* Quota is informational; keep the last value. */ }
        }
        return { signed_in: signedIn, email, login_url: loginUrl ?? null, error, quota: signedIn ? quota ?? null : null };
    }
    if (method === 'auth.login') {
        if (c.auth.isLoggedIn()) return { signed_in: true };
        if (login) return { login_url: loginUrl ?? null };
        authError = undefined; authController = new AbortController();
        const urlReady = Promise.withResolvers<string>();
        login = c.auth.authViaWeb(url => { loginUrl = url; urlReady.resolve(url); }, authController.signal)
            .then(() => { loginUrl = undefined; })
            .catch(error => { authError = error instanceof Error ? error.message : 'Sign-in failed'; loginUrl = undefined; urlReady.reject(new Error(authError)); })
            .finally(() => { login = undefined; });
        return { login_url: await urlReady.promise };
    }
    if (method === 'auth.logout') {
        authController?.abort(); await login; await c.auth.logout();
        await Promise.all([c.caches.cryptoCache.clear(), c.caches.entitiesCache.clear(), c.index.clear()]);
        loginUrl = undefined; authError = undefined; return {};
    }
    c.assertReady();
    switch (method) {
        case 'browse': return drive!.browse(p.uid);
        case 'search': return drive!.search(typeof p.query === 'string' ? p.query : '');
        case 'snapshot': return drive!.snapshot(requireString(p.root), p.selection ?? undefined);
        case 'inspect': return drive!.inspect(requireString(p.root), requireString(p.path));
        case 'upload': return drive!.upload(requireString(p.root), requireString(p.path), requireString(p.source), p.expected ?? null, requireString(p.hash), transfer(id, p));
        case 'download': return drive!.download(p.entry, requireString(p.destination), transfer(id, p)).then(() => ({}));
        case 'mkdir': return drive!.mkdir(requireString(p.root), requireString(p.path));
        case 'revisions': return drive!.revisions(requireString(p.root), requireString(p.path));
        case 'restoreRevision': return drive!.restoreRevision(requireString(p.root), requireString(p.path), requireString(p.revision));
        case 'move': return drive!.move(requireString(p.root), requireString(p.from), requireString(p.to), p.expected);
        case 'trash': return drive!.trash(requireString(p.root), requireString(p.path), p.expected).then(() => ({}));
        default: throw new Error('Unknown adapter method');
    }
}
const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
// Requests run concurrently; the service only overlaps transfers and their checks.
const running = new Set<Promise<void>>();
input.on('line', line => {
    const task = (async () => {
        let id: unknown = null;
        try {
            if (line.length > 1024 * 1024) throw new Error('Request too large');
            const request = JSON.parse(line); id = request.id;
            if (!Number.isSafeInteger(id)) throw new Error('Invalid request ID');
            const result = await dispatch(requireString(request.method), request.params ?? {}, id);
            send({ id, result });
        } catch (error) {
            // Do not emit stack traces, session objects, or full request payloads.
            const kind = classify(error);
            send({ id, error: { message: error instanceof Error ? error.message : 'SDK operation failed', kind, retryable: kind !== 'fatal' } });
        } finally { lastProgress.delete(id); }
    })();
    running.add(task); void task.finally(() => running.delete(task));
});
input.on('close', () => { authController?.abort(); void Promise.allSettled([...running]).finally(() => process.exit(0)); });
