import { createWriteStream } from 'node:fs';
import { Writable } from 'node:stream';
import { stat, unlink, utimes } from 'node:fs/promises';
import path from 'node:path';
import { DriveEventType, NodeType, RevisionState, type NodeEntity } from '@protontech/drive-sdk';
import type { Client } from './client';
import { relativePath, requireExpected, type Entry } from './protocol';
import { Throttle } from './throttle';

/** `unsupported` nodes stay indexed so events never stall; they only matter inside the selection. */
type IndexedNode = { parent?: string; name: string; entry: Entry; unsupported?: { reason: string; skip: boolean } };
type Index = { root: string; scope: string; cursor: string; nodes: Record<string, IndexedNode> };
const DOCUMENT = 'Proton collaborative documents cannot be synced as ordinary files yet.';
const validName = (name: string) => { relativePath(name); if (name.includes('/')) throw new Error('Remote filename contains a slash'); return name; };

export function convert(node: NodeEntity): IndexedNode {
    if (!node.name.ok || node.errors?.length) throw new Error('A remote item could not be decrypted or verified. Sync stopped.');
    if (node.type !== NodeType.File && node.type !== NodeType.Folder) throw new Error('Unsupported Proton document or file type');
    if (node.mediaType?.startsWith('application/vnd.proton')) throw new Error(DOCUMENT);
    const revision = node.activeRevision;
    if (node.type === NodeType.File && !revision) throw new Error('Remote file has no active revision');
    return { parent: node.parentUid, name: validName(node.name.value), entry: {
        uid: node.uid, kind: node.type, version: revision?.uid ?? 'directory',
        size: revision?.claimedSize ?? 0,
        hash: revision?.claimedDigests?.sha1Verified ? revision.claimedDigests.sha1 ?? null : null,
    } };
}

export function indexed(node: NodeEntity): IndexedNode {
    try { return convert(node); } catch (error) {
        const reason = error instanceof Error ? error.message : 'Unsupported remote item';
        // Documents are skipped. Undecryptable or invalid items block sync where they are in scope,
        // because hiding them would look like a remote deletion.
        const skip = reason === DOCUMENT || reason === 'Unsupported Proton document or file type';
        return { parent: node.parentUid, name: node.name.ok ? node.name.value : '', unsupported: { reason, skip },
            entry: { uid: node.uid, kind: node.type === NodeType.Folder ? 'folder' : 'file', version: '', size: 0, hash: null } };
    }
}

/** Per-transfer options: a progress callback and a shared rate limit in bytes per second. */
export type Transfer = { progress?: (bytes: number) => void; limit?: number };

export class Drive {
    private lastRefresh = new Map<string, number>();
    /** Index writes are read-modify-write, so concurrent transfers take turns. */
    private indexLock = Promise.resolve();
    readonly throttles = { upload: new Throttle(), download: new Throttle() };
    constructor(private client: Client) {}
    private async root(uid: string) {
        this.client.assertReady();
        const node = await this.client.sdk.getNode(uid);
        if (node.type !== NodeType.Folder || node.trashTime) throw new Error('Remote sync folder is unavailable or in Trash.');
        const ownRoot = await this.client.sdk.getMyFilesRootFolder();
        if (node.uid !== ownRoot.uid) {
            const hierarchy = await this.client.sdk.getNodeHierarchy(uid);
            if (!hierarchy.some(n => n.uid === ownRoot.uid)) throw new Error('Only folders inside My files are supported initially.');
        }
        return node;
    }
    /** The saved event index for My files, if a sync has built one. Never crawls. */
    private async savedIndex(): Promise<Index | undefined> {
        const own = await this.client.sdk.getMyFilesRootFolder();
        try { return JSON.parse(await this.client.index.getEntity(`root:${own.uid}`)); }
        catch { return undefined; }
    }
    async browse(uid?: string) {
        this.client.assertReady();
        const root = uid ? await this.root(uid) : await this.client.sdk.getMyFilesRootFolder();
        const sizes = folderSizes(await this.savedIndex());
        const children = [];
        for await (const node of this.client.sdk.iterateFolderChildren(root.uid)) {
            if (!node.trashTime) {
                const supported = node.name.ok && !node.errors?.length
                    && (node.type === NodeType.File || node.type === NodeType.Folder)
                    && !node.mediaType?.startsWith('application/vnd.proton');
                children.push({ uid: node.uid, name: node.name.ok ? node.name.value : 'Unable to decrypt name',
                    kind: node.type, size: node.type === NodeType.Folder ? sizes.get(node.uid) ?? null : node.activeRevision?.claimedSize ?? 0,
                    supported, document: !!node.mediaType?.startsWith('application/vnd.proton') });
            }
        }
        const ancestors = (await this.client.sdk.getNodeHierarchy(root.uid)).map(n => n.uid);
        const own = !uid || root.uid === (await this.client.sdk.getMyFilesRootFolder()).uid;
        return { uid: root.uid, name: own || !root.name.ok ? 'My files' : root.name.value, children, ancestors };
    }
    /** Name search across My files, using the event index (built on first use). */
    async search(query: string) {
        this.client.assertReady();
        const needle = query.trim().toLowerCase();
        if (!needle) return [];
        const own = await this.client.sdk.getMyFilesRootFolder();
        let index = await this.savedIndex();
        if (!index) { index = await this.fresh(own); await this.client.index.setEntity(`root:${own.uid}`, JSON.stringify(index)); }
        const sizes = folderSizes(index);
        const results = [];
        for (const [uid, node] of Object.entries(index.nodes)) {
            if (!node.name.toLowerCase().includes(needle)) continue;
            const parts = [node.name]; let parent = node.parent; let ok = true;
            for (let depth = 0; parent !== own.uid; depth++) {
                const up = parent ? index.nodes[parent] : undefined;
                if (!up || depth > 256) { ok = false; break; }
                parts.unshift(up.name); parent = up.parent;
            }
            if (!ok) continue;
            results.push({ uid, name: node.name, path: parts.join('/'), kind: node.entry.kind,
                size: node.entry.kind === 'folder' ? sizes.get(uid) ?? null : node.entry.size, supported: !node.unsupported });
            if (results.length >= 200) break;
        }
        return results.sort((a, b) => a.path.localeCompare(b.path));
    }
    private async crawl(index: Index, uid: string) {
        for await (const node of this.client.sdk.iterateFolderChildren(uid)) {
            if (node.trashTime) continue;
            index.nodes[node.uid] = indexed(node);
            if (node.type === NodeType.Folder) await this.crawl(index, node.uid);
        }
    }
    private remove(index: Index, uid: string) {
        for (const [id, n] of Object.entries(index.nodes)) if (n.parent === uid) this.remove(index, id);
        delete index.nodes[uid];
    }
    private async fresh(root: NodeEntity): Promise<Index> {
        let cursor = '';
        for await (const event of this.client.sdk.iterateEvents(root.treeEventScopeId)) cursor = event.eventId;
        if (!cursor) throw new Error('Could not establish a remote event cursor');
        const index: Index = { root: root.uid, scope: root.treeEventScopeId, cursor, nodes: Object.create(null) };
        await this.crawl(index, root.uid);
        return index;
    }
    async snapshot(uid: string, selection?: string[]): Promise<Record<string, Entry>> {
        if (selection !== undefined) {
            if (!Array.isArray(selection)) throw new Error('Invalid sync selection');
            for (const p of selection) relativePath(p);
        }
        let tree: Record<string, Entry> = Object.create(null);
        // Build the tree before saving, so a bad remote name never advances the cursor.
        await this.current(uid, 30_000, index => { tree = buildTree(index, uid, selection); });
        return tree;
    }
    /** Serialize index reads and writes; parallel transfers update it concurrently. */
    private locked<T>(work: () => Promise<T>): Promise<T> {
        const turn = this.indexLock.then(work);
        this.indexLock = turn.then(() => {}, () => {});
        return turn;
    }
    /**
     * The event index for `uid`, with Drive events applied if they were last read more than
     * `maxAge` ms ago. Applying events also refreshes the SDK's node cache.
     */
    private current(uid: string, maxAge: number, check?: (index: Index) => void): Promise<Index> {
        return this.locked(async () => {
            const root = await this.root(uid);
            let index: Index;
            try { index = JSON.parse(await this.client.index.getEntity(`root:${uid}`)); }
            catch (e) {
                // Missing index is expected; corrupt or inaccessible storage must not masquerade as an empty remote.
                if (!(e instanceof Error) || !e.message.toLowerCase().includes('not found')) throw e;
                index = await this.fresh(root);
            }
            if (!index.cursor || index.root !== uid) throw new Error('Invalid remote index. Recovery is required.');
            let refresh = false;
            const shouldPoll = Date.now() - (this.lastRefresh.get(uid) ?? 0) >= maxAge;
            if (shouldPoll) for await (const event of this.client.sdk.iterateEvents(index.scope, index.cursor)) {
                if (event.type === DriveEventType.TreeRemove) throw new Error('Access to the remote drive was lost. Local files are preserved.');
                if (event.type === DriveEventType.TreeRefresh) { refresh = true; break; }
                if ('nodeUid' in event) {
                    const known = !!index.nodes[event.nodeUid];
                    const parentInside = event.parentNodeUid === uid || !!index.nodes[event.parentNodeUid ?? ''];
                    if (event.nodeUid === uid && (event.type === DriveEventType.NodeDeleted || ('isTrashed' in event && event.isTrashed))) {
                        throw new Error('Remote sync root was removed. Local files are preserved.');
                    }
                    if (event.type === DriveEventType.NodeDeleted || ('isTrashed' in event && event.isTrashed)) this.remove(index, event.nodeUid);
                    else if (known || parentInside) {
                        const node = await this.client.sdk.getNode(event.nodeUid);
                        if (node.parentUid !== uid && !index.nodes[node.parentUid ?? '']) this.remove(index, node.uid);
                        else {
                            index.nodes[node.uid] = indexed(node);
                            if (!known && node.type === NodeType.Folder) await this.crawl(index, node.uid);
                        }
                    }
                }
                index.cursor = event.eventId;
            }
            if (refresh) index = await this.fresh(root);
            check?.(index);
            // The encrypted cache writes the node index and its cursor as a single SQLite value.
            await this.client.index.setEntity(`root:${uid}`, JSON.stringify(index));
            if (shouldPoll) this.lastRefresh.set(uid, Date.now());
            return index;
        });
    }
    private remember(root: string, node: NodeEntity | null, removedUid?: string) {
        return this.locked(() => this.rememberNow(root, node, removedUid));
    }
    private async rememberNow(root: string, node: NodeEntity | null, removedUid?: string) {
        let index: Index;
        try { index = JSON.parse(await this.client.index.getEntity(`root:${root}`)); }
        catch (e) { if (e instanceof Error && e.message.includes('not found')) return; throw e; }
        if (removedUid) this.remove(index, removedUid);
        if (node) index.nodes[node.uid] = indexed(node);
        await this.client.index.setEntity(`root:${root}`, JSON.stringify(index));
    }
    /**
     * Find the node at a path just before changing it: apply pending Drive events (at most one
     * request every 2 seconds), look the path up in the index, then fetch only that node.
     * This replaces listing every folder along the path.
     */
    private async resolve(root: string, relative: string): Promise<NodeEntity | null> {
        relativePath(relative);
        const index = await this.current(root, 2_000);
        let parent = root;
        let found: string | undefined;
        for (const part of relative.split('/')) {
            found = undefined;
            for (const [uid, node] of Object.entries(index.nodes)) {
                if (node.parent !== parent) continue;
                if (!node.name) throw new Error('Remote filename could not be decrypted');
                if (node.name === part) { if (found) throw new Error('Ambiguous remote name'); found = uid; }
            }
            if (!found) return null;
            parent = found;
        }
        const node = await this.client.sdk.getNode(found!);
        // The index and the node agree unless Drive changed in the last instant; then stop safely.
        if (node.trashTime) return null;
        if (!node.name.ok || node.name.value !== path.posix.basename(relative) || node.parentUid !== index.nodes[found!].parent) {
            throw new Error('Remote item changed since planning. Review and retry.');
        }
        return node;
    }
    async inspect(root: string, relative: string): Promise<Entry | null> {
        const node = await this.resolve(root, relative); return node ? convert(node).entry : null;
    }
    private async parent(root: string, relative: string) {
        relativePath(relative); const dirname = path.posix.dirname(relative);
        const parent = dirname === '.' ? await this.root(root) : await this.resolve(root, dirname);
        if (!parent || parent.type !== NodeType.Folder) throw new Error('Remote parent folder is unavailable');
        return parent;
    }
    async upload(root: string, relative: string, source: string, expected: Entry | null, hash: string, transfer: Transfer = {}): Promise<Entry> {
        requireExpected(await this.inspect(root, relative), expected);
        const file = Bun.file(source); const meta = await stat(source);
        if (!meta.isFile()) throw new Error('Upload source is not a regular file');
        const metadata = { mediaType: Bun.file(relative).type || 'application/octet-stream', expectedSize: meta.size, expectedSha1: hash, modificationTime: meta.mtime };
        const uploader = expected
            ? await this.client.sdk.getFileRevisionUploader(expected.uid, metadata)
            : await this.client.sdk.getFileUploader((await this.parent(root, relative)).uid, path.posix.basename(relative), metadata);
        this.throttles.upload.rate = transfer.limit;
        const controller = await uploader.uploadFromStream(this.throttles.upload.readable(file.stream()), [], transfer.progress);
        const result = await controller.completion();
        const node = await this.client.sdk.getNode(result.nodeUid);
        if (node.activeRevision?.uid !== result.nodeRevisionUid) {
            throw new Error('Remote file changed immediately after upload. Review changes before continuing.');
        }
        await this.remember(root, node);
        const entry = convert(node).entry;
        entry.hash = hash;
        return entry;
    }
    async download(entry: Entry, destination: string, transfer: Transfer = {}) {
        this.client.assertReady();
        const downloader = await this.client.sdk.getFileRevisionDownloader(entry.version);
        const output = createWriteStream(destination, { flags: 'wx', mode: 0o600 });
        this.throttles.download.rate = transfer.limit;
        const stream = this.throttles.download.writable(Writable.toWeb(output).getWriter());
        const controller = downloader.downloadToStream(stream, transfer.progress);
        try {
            await controller.completion();
            if (controller.isDownloadCompleteWithSignatureIssues()) throw new Error('Download signature verification failed');
            // The SDK releases the stream without closing it; closing flushes the file.
            await stream.close();
            // Keep the file's own modification time; the final rename preserves it.
            const modified = await this.claimedModificationTime(entry);
            if (modified) await utimes(destination, modified, modified);
        } catch (error) { output.destroy(); await unlink(destination).catch(() => {}); throw error; }
    }
    /** The modification time the uploader recorded for this revision, if it is still current. */
    private async claimedModificationTime(entry: Entry): Promise<Date | undefined> {
        try {
            const revision = (await this.client.sdk.getNode(entry.uid)).activeRevision;
            const time = revision?.uid === entry.version ? revision.claimedModificationTime : undefined;
            return time instanceof Date && !Number.isNaN(time.getTime()) ? time : undefined;
        } catch { return undefined; }
    }
    async mkdir(root: string, relative: string): Promise<Entry> {
        requireExpected(await this.inspect(root, relative), null);
        const node = await this.client.sdk.createFolder((await this.parent(root, relative)).uid, path.posix.basename(relative));
        await this.remember(root, node);
        return convert(node).entry;
    }
    async trash(root: string, relative: string, expected: Entry) {
        requireExpected(await this.inspect(root, relative), expected);
        if (expected.kind === 'folder') {
            for await (const node of this.client.sdk.iterateFolderChildren(expected.uid)) if (!node.trashTime) throw new Error('Remote folder gained contents; deletion held.');
        }
        for await (const result of this.client.sdk.trashNodes([expected.uid])) if (!result.ok) throw result.error;
        await this.remember(root, null, expected.uid);
    }
    private async file(root: string, relative: string) {
        const node = await this.resolve(root, relative);
        if (!node || node.type !== NodeType.File) throw new Error('This file is no longer in Proton Drive.');
        return node;
    }
    /** A file's versions in Drive, newest first. */
    async revisions(root: string, relative: string) {
        const node = await this.file(root, relative);
        const list = [];
        for await (const r of this.client.sdk.iterateRevisions(node.uid)) {
            list.push({ uid: r.uid, time: Math.floor(r.creationTime.getTime() / 1000),
                size: r.claimedSize ?? r.storageSize, active: r.state === RevisionState.Active });
        }
        return list;
    }
    /** Make an older version current. Drive keeps the replaced one as a version too. */
    async restoreRevision(root: string, relative: string, revision: string) {
        const node = await this.file(root, relative);
        const found = (await this.revisions(root, relative)).find(r => r.uid === revision);
        if (!found) throw new Error('That version is no longer available.');
        if (found.active) return {};
        await this.client.sdk.restoreRevision(revision);
        await this.remember(root, await this.client.sdk.getNode(node.uid));
        return {};
    }
    /** Move and/or rename a node. Drive keeps its uid and revisions, so history survives. */
    async move(root: string, from: string, to: string, expected: Entry): Promise<Entry> {
        requireExpected(await this.inspect(root, from), expected);
        requireExpected(await this.inspect(root, to), null);
        const { dirname, basename, join } = path.posix;
        const rename = async () => { await this.client.sdk.renameNode(expected.uid, basename(to)); };
        const move = async () => {
            const parent = await this.parent(root, to);
            for await (const result of this.client.sdk.moveNodes([expected.uid], parent.uid)) if (!result.ok) throw result.error;
        };
        // Moving keeps the old name, so take the order whose intermediate name is free.
        if (dirname(from) === dirname(to)) await rename();
        else if (basename(from) === basename(to)) await move();
        else if (!await this.inspect(root, join(dirname(to), basename(from)))) { await move(); await rename(); }
        else if (!await this.inspect(root, join(dirname(from), basename(to)))) { await rename(); await move(); }
        else throw new Error('Both the old and new names are taken in Drive; rename one item and retry.');
        const node = await this.client.sdk.getNode(expected.uid);
        await this.remember(root, node);
        const entry = convert(node).entry;
        entry.hash = expected.hash;
        return entry;
    }
}

/** Paths for the selected part of the index. Selection covers ancestors (as scaffolding) and descendants. */
export function buildTree(index: Index, root: string, selection?: string[]): Record<string, Entry> {
    const children = new Map<string, string[]>();
    for (const [uid, node] of Object.entries(index.nodes)) {
        const parent = node.parent ?? '';
        if (parent !== root && !index.nodes[parent]) throw new Error('Remote folder hierarchy is incomplete');
        children.set(parent, [...children.get(parent) ?? [], uid]);
    }
    const within = (p: string) => !selection || selection.some(s => p === s || p.startsWith(s + '/'));
    const relevant = (p: string) => within(p) || selection!.some(s => s.startsWith(p + '/'));
    const tree: Record<string, Entry> = Object.create(null);
    const seen = new Set<string>();
    const visit = (folder: string, prefix: string) => {
        if (seen.has(folder)) throw new Error('Remote folder hierarchy contains a cycle');
        seen.add(folder);
        for (const uid of children.get(folder) ?? []) {
            const node = index.nodes[uid];
            const p = prefix + node.name;
            // An undecryptable name could be a selected item, so it always blocks in a visited folder.
            if (node.name && !relevant(p)) continue;
            if (node.unsupported) {
                if (!node.unsupported.skip || selection?.includes(p)) throw new Error(node.unsupported.reason);
                continue;
            }
            relativePath(p);
            if (Object.hasOwn(tree, p)) throw new Error('Two remote items have the same local path');
            tree[p] = node.entry;
            if (node.entry.kind === 'folder') visit(uid, p + '/');
        }
    };
    visit(root, '');
    return tree;
}

/** Total file size below each indexed folder. */
export function folderSizes(index?: Index): Map<string, number> {
    const sizes = new Map<string, number>();
    if (!index) return sizes;
    for (const node of Object.values(index.nodes)) {
        if (node.entry.kind !== 'file' || node.unsupported) continue;
        const seen = new Set<string>();
        for (let parent = node.parent; parent && index.nodes[parent] && !seen.has(parent); parent = index.nodes[parent].parent) {
            seen.add(parent);
            sizes.set(parent, (sizes.get(parent) ?? 0) + node.entry.size);
        }
    }
    return sizes;
}
