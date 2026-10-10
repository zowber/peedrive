import { expect, test } from 'bun:test';
import { NodeType } from '@protontech/drive-sdk';
import { Drive } from '../src/drive';
import type { Client } from '../src/client';
import { mkdtempSync, writeFileSync, rmSync, statSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

function fixture() {
    const saved = new Map<string, string>();
    const node = (uid: string, name: string, parentUid: string, folder = false) => ({
        uid, name: { ok: true, value: name }, parentUid, type: folder ? 'folder' : 'file', treeEventScopeId: 'scope',
        ...(!folder && { activeRevision: { uid: `revision-${uid}`, claimedSize: 3, claimedDigests: { sha1: 'a'.repeat(40), sha1Verified: true } } }),
    });
    const root = node('root', 'My files', '', true);
    const nodes = new Map<string, any>([['root', root], ['a', node('a', 'a.txt', 'root')]]);
    let events: any[] = [];
    let failAfterEvents = false;
    let listings = 0;
    const calls: string[] = [];
    const sdk = {
        async getNode(uid: string) { if (!nodes.has(uid)) throw new Error('Missing node'); return nodes.get(uid); },
        async getMyFilesRootFolder() { return root; },
        async getNodeHierarchy() { return [root]; },
        async *iterateFolderChildren(uid: string) { listings++; for (const n of nodes.values()) if (n.parentUid === uid) yield n; },
        async *iterateEvents(_scope: string, cursor?: string) {
            if (!cursor) { yield { type: 'fast_forward', eventId: 'initial' }; return; }
            for (const e of events) yield e;
            if (failAfterEvents) throw new Error('Network failure during events');
        },
        async createFolder(parentUid: string, name: string) { const n = node(`new-${name}`, name, parentUid, true); nodes.set(n.uid, n); return n; },
        async *trashNodes(uids: string[]) { for (const uid of uids) { nodes.delete(uid); yield { ok: true, uid }; } },
        async *iterateRevisions(uid: string) {
            const active = nodes.get(uid).activeRevision.uid;
            for (const r of [active, 'revision-old']) yield { uid: r, state: r === active ? 'active' : 'superseded', creationTime: new Date(r === active ? 2_000_000 : 1_000_000), storageSize: 9, claimedSize: 3 };
        },
        async restoreRevision(uid: string) { calls.push(`restore ${uid}`); nodes.set('a', { ...nodes.get('a'), activeRevision: { ...nodes.get('a').activeRevision, uid } }); },
        async renameNode(uid: string, name: string) { calls.push(`rename ${name}`); const n = { ...nodes.get(uid), name: { ok: true, value: name } }; nodes.set(uid, n); return n; },
        async *moveNodes(uids: string[], parentUid: string) { for (const uid of uids) { calls.push(`move ${parentUid}`); nodes.set(uid, { ...nodes.get(uid), parentUid }); yield { ok: true, uid }; } },
    };
    const client = { sdk, assertReady() {}, index: {
        async getEntity(key: string) { if (!saved.has(key)) throw new Error('Entity not found'); return saved.get(key); },
        async setEntity(key: string, value: string) { saved.set(key, value); },
    } } as unknown as Client;
    const drive = () => new Drive(client);
    return { client, saved, nodes, node, drive, events: (value: any[]) => { events = value; }, fail: () => { failAfterEvents = true; }, listings: () => listings, calls };
}
test('initial index and cursor persist together; restart uses events without a recursive scan', async () => {
    const f = fixture(); expect((await f.drive().snapshot('root'))['a.txt'].uid).toBe('a');
    expect(JSON.parse(f.saved.get('root:root')!).cursor).toBe('initial');
    const listings = f.listings(); await f.drive().snapshot('root'); expect(f.listings()).toBe(listings);
});
test('event failure leaves the previous durable cursor and tree intact', async () => {
    const f = fixture(); await f.drive().snapshot('root'); const before = f.saved.get('root:root');
    f.nodes.set('b', f.node('b', 'b.txt', 'root'));
    f.events([{ type: 'node_created', nodeUid: 'b', parentNodeUid: 'root', eventId: 'next' }]); f.fail();
    await expect(f.drive().snapshot('root')).rejects.toThrow('Network failure'); expect(f.saved.get('root:root')).toBe(before);
});
test('remote folder rename changes descendant paths without recursive listing', async () => {
    const f = fixture(); f.nodes.set('folder', f.node('folder', 'old', 'root', true)); f.nodes.set('inside', f.node('inside', 'file.txt', 'folder'));
    await f.drive().snapshot('root'); const listings = f.listings();
    f.nodes.set('folder', f.node('folder', 'new', 'root', true)); f.events([{ type: 'node_updated', nodeUid: 'folder', parentNodeUid: 'root', eventId: 'renamed' }]);
    const tree = await f.drive().snapshot('root'); expect(tree['new/file.txt']).toBeDefined(); expect(tree['old/file.txt']).toBeUndefined(); expect(f.listings()).toBe(listings);
});
test('access loss never returns an empty remote tree', async () => {
    const f = fixture(); await f.drive().snapshot('root'); f.events([{ type: 'tree_remove', eventId: 'none' }]);
    await expect(f.drive().snapshot('root')).rejects.toThrow('Access');
});
test('event gap triggers a controlled rescan', async () => {
    const f = fixture(); await f.drive().snapshot('root'); f.nodes.set('b', f.node('b', 'b.txt', 'root')); f.events([{ type: 'tree_refresh', eventId: 'gap' }]);
    expect((await f.drive().snapshot('root'))['b.txt']).toBeDefined();
});
test('own folder creation remains indexed before its event arrives', async () => {
    const f = fixture(); const d = f.drive(); await d.snapshot('root'); await d.mkdir('root', 'created');
    expect((await d.snapshot('root'))['created']).toBeDefined();
});
test('own deletion is reflected before its event arrives', async () => {
    const f = fixture(); const d = f.drive(); const tree = await d.snapshot('root'); await d.trash('root', 'a.txt', tree['a.txt']);
    expect((await d.snapshot('root'))['a.txt']).toBeUndefined();
});
test('bad remote names abort indexing without cursor advancement', async () => {
    const f = fixture(); await f.drive().snapshot('root'); const before = f.saved.get('root:root');
    f.nodes.set('bad', f.node('bad', '../outside', 'root')); f.events([{ type: 'node_created', nodeUid: 'bad', parentNodeUid: 'root', eventId: 'bad' }]);
    await expect(f.drive().snapshot('root')).rejects.toThrow(); expect(f.saved.get('root:root')).toBe(before);
});

test('valid filenames that match JavaScript object properties are indexed normally', async () => {
    const f=fixture();
    for (const name of ['__proto__','constructor','toString']) f.nodes.set(`node-${name}`,f.node(`node-${name}`,name,'root'));
    const tree=await f.drive().snapshot('root');
    const serialized=JSON.parse(JSON.stringify(tree));
    for (const name of ['__proto__','constructor','toString']) expect(serialized[name].uid).toBe(`node-${name}`);
});

for (const raced of [false,true]) test(`upload revision identity ${raced?'rejects a concurrent winner':'accepts our completed revision'}`,async()=>{
    const f=fixture();const drive=f.drive();await drive.snapshot('root');
    const before=f.saved.get('root:root');
    const temp=mkdtempSync(path.join(tmpdir(),'peedrive-upload-test-'));
    const source=path.join(temp,'source');writeFileSync(source,'uploaded content');
    (f.client.sdk as any).getFileUploader=async()=>({uploadFromStream:async()=>({completion:async()=>{
        const node=f.node('uploaded','new.txt','root');
        if(raced) node.activeRevision!.uid='revision-other-writer';
        f.nodes.set('uploaded',node);
        return {nodeUid:'uploaded',nodeRevisionUid:'revision-uploaded'};
    }})});
    try {
        const upload=drive.upload('root','new.txt',source,null,'b'.repeat(40));
        if(raced){await expect(upload).rejects.toThrow('changed immediately after upload');expect(f.saved.get('root:root')).toBe(before);}
        else {const entry=await upload;expect(entry.version).toBe('revision-uploaded');expect(entry.hash).toBe('b'.repeat(40));}
    } finally {rmSync(temp,{recursive:true,force:true});}
});

test('browse lists folders, ordinary files and unsupported documents without syncing them', async () => {
    const f=fixture(); f.nodes.set('folder',f.node('folder','Documents','root',true));
    f.nodes.set('doc',{...f.node('doc','Online document','root'),mediaType:'application/vnd.proton.doc'});
    const list=await f.drive().browse();
    expect(list.children.map(c=>c.name)).toContain('a.txt');
    expect(list.children.find(c=>c.uid==='folder')?.kind).toBe(NodeType.Folder);
    expect(list.children.find(c=>c.uid==='doc')?.supported).toBe(false);
});
test('selective snapshot skips unsupported unselected files and unselected subtrees', async () => {
    const f=fixture(); f.nodes.set('doc',{...f.node('doc','Online document','root'),mediaType:'application/vnd.proton.doc'});
    f.nodes.set('folder',f.node('folder','Other','root',true));
    const tree=await f.drive().snapshot('root',['a.txt']);
    expect(Object.keys(tree)).toEqual(['a.txt']);
    const listings=f.listings(); await f.drive().snapshot('root',['a.txt']);
    expect(f.listings()).toBe(listings);
    await expect(f.drive().snapshot('root',['Online document'])).rejects.toThrow('collaborative');
});
test('documents inside a selected folder or arriving by event do not stop sync', async () => {
    const f=fixture(); f.nodes.set('folder',f.node('folder','Docs','root',true));
    f.nodes.set('doc',{...f.node('doc','Notes','folder'),mediaType:'application/vnd.proton.doc'});
    expect(Object.keys(await f.drive().snapshot('root',['Docs']))).toEqual(['Docs']);
    expect(Object.keys(await f.drive().snapshot('root')).sort()).toEqual(['Docs','a.txt']);
    f.nodes.set('sheet',{...f.node('sheet','Budget','root'),mediaType:'application/vnd.proton.sheet'});
    f.events([{ type: 'node_created', nodeUid: 'sheet', parentNodeUid: 'root', eventId: 'sheet' }]);
    expect(Object.keys(await f.drive().snapshot('root')).sort()).toEqual(['Docs','a.txt']);
});
test('undecryptable items block sync only where they are in scope', async () => {
    const f=fixture(); f.nodes.set('other',f.node('other','Other','root',true)); f.nodes.set('mine',f.node('mine','Mine','root',true));
    f.nodes.set('bad',{...f.node('bad','x','other'),name:{ok:false}});
    expect(Object.keys(await f.drive().snapshot('root',['Mine']))).toEqual(['Mine']);
    await expect(f.drive().snapshot('root',['Other'])).rejects.toThrow('decrypted');
    f.nodes.set('top',{...f.node('top','y','root'),name:{ok:false}});
    f.events([{ type: 'node_created', nodeUid: 'top', parentNodeUid: 'root', eventId: 'top' }]);
    await expect(f.drive().snapshot('root',['Mine'])).rejects.toThrow('decrypted');
});
test('selective snapshot includes ancestors and descendants with exact path boundaries', async () => {
    const f=fixture(); f.nodes.set('folder',f.node('folder','docs','root',true));
    f.nodes.set('inside',f.node('inside','chosen.txt','folder'));
    f.nodes.set('other',f.node('other','other.txt','folder'));
    expect(Object.keys(await f.drive().snapshot('root',['docs/chosen.txt']))).toEqual(['docs','docs/chosen.txt']);
    expect(Object.keys(await f.drive().snapshot('root',[]))).toEqual([]);
});

test('browse reports folder sizes from the index and search finds nested items', async () => {
    const f=fixture(); f.nodes.set('folder',f.node('folder','Docs','root',true)); f.nodes.set('deep',f.node('deep','Report.pdf','folder'));
    (f.client.sdk as any).getNodeHierarchy=async()=>[]; const d=f.drive();
    expect((await d.browse()).children.find(c=>c.uid==='folder')?.size).toBeNull();
    await d.snapshot('root');
    expect((await d.browse()).children.find(c=>c.uid==='folder')?.size).toBe(3);
    expect((await d.search('report')).map(r=>r.path)).toEqual(['Docs/Report.pdf']);
    expect(await d.search('  ')).toEqual([]);
});

test('move renames in place, moves between folders, and keeps the node and revision', async () => {
    const f = fixture(); f.nodes.set('docs', f.node('docs', 'docs', 'root', true)); const drive = f.drive();
    const before = (await drive.snapshot('root'))['a.txt'];
    const renamed = await drive.move('root', 'a.txt', 'b.txt', before);
    expect(renamed).toEqual(before); expect(f.calls).toEqual(['rename b.txt']);
    await drive.move('root', 'b.txt', 'docs/c.txt', renamed);
    expect(f.calls).toEqual(['rename b.txt', 'move docs', 'rename c.txt']);
    expect(Object.keys(await drive.snapshot('root'))).toEqual(['docs', 'docs/c.txt']);
});
test('move refuses a changed source or an occupied destination', async () => {
    const f = fixture(); f.nodes.set('b', f.node('b', 'b.txt', 'root')); const drive = f.drive();
    const tree = await drive.snapshot('root');
    await expect(drive.move('root', 'a.txt', 'b.txt', tree['a.txt'])).rejects.toThrow('changed');
    await expect(drive.move('root', 'a.txt', 'c.txt', { ...tree['a.txt'], version: 'other' })).rejects.toThrow('changed');
    expect(f.calls).toEqual([]);
});

test('concurrent uploads both reach the saved index', async () => {
    const f = fixture(); const drive = f.drive(); await drive.snapshot('root');
    const temp = mkdtempSync(path.join(tmpdir(), 'peedrive-upload-test-'));
    const source = path.join(temp, 'source'); writeFileSync(source, 'content');
    // A slow index store lets unsynchronised read-modify-write cycles overlap.
    const index = f.client.index as any; const { getEntity, setEntity } = index;
    index.getEntity = async (key: string) => { await Bun.sleep(5); return getEntity(key); };
    index.setEntity = async (key: string, value: string) => { await Bun.sleep(5); return setEntity(key, value); };
    (f.client.sdk as any).getFileUploader = async (_parent: string, name: string) => ({ uploadFromStream: async () => ({ completion: async () => {
        const node = f.node(name, name, 'root'); f.nodes.set(name, node);
        return { nodeUid: name, nodeRevisionUid: `revision-${name}` };
    } }) });
    try {
        await Promise.all(['one.txt', 'two.txt', 'three.txt'].map(n => drive.upload('root', n, source, null, 'c'.repeat(40))));
        const index = JSON.parse(f.saved.get('root:root')!);
        expect(Object.keys(index.nodes).sort()).toEqual(['a', 'one.txt', 'three.txt', 'two.txt']);
    } finally { rmSync(temp, { recursive: true, force: true }); }
});

test('versions list newest first and restoring one makes it current in the index', async () => {
    const f = fixture(); const drive = f.drive(); await drive.snapshot('root');
    expect(await drive.revisions('root', 'a.txt')).toEqual([
        { uid: 'revision-a', time: 2000, size: 3, active: true }, { uid: 'revision-old', time: 1000, size: 3, active: false }]);
    await drive.restoreRevision('root', 'a.txt', 'revision-a'); expect(f.calls).toEqual([]);
    await expect(drive.restoreRevision('root', 'a.txt', 'revision-elsewhere')).rejects.toThrow('no longer available');
    await drive.restoreRevision('root', 'a.txt', 'revision-old');
    expect(f.calls).toEqual(['restore revision-old']);
    expect(JSON.parse(f.saved.get('root:root')!).nodes.a.entry.version).toBe('revision-old');
    await expect(drive.revisions('root', 'missing.txt')).rejects.toThrow('no longer in Proton Drive');
});

for (const current of [true,false]) test(`downloads ${current?'keep':'skip'} the uploader's modification time ${current?'':'for a superseded revision'}`, async () => {
    const f=fixture(); const then=new Date('2020-09-13T12:26:40Z');
    const file=f.nodes.get('a'); file.activeRevision={...file.activeRevision, claimedModificationTime: then};
    (f.client.sdk as any).getFileRevisionDownloader=async()=>({
        downloadToStream:(stream: WritableStream<Uint8Array>)=>({ completion: async()=>{ const w=stream.getWriter(); await w.write(new TextEncoder().encode('abc')); w.releaseLock(); },
            isDownloadCompleteWithSignatureIssues: ()=>false }),
    });
    const temp=mkdtempSync(path.join(tmpdir(),'peedrive-download-test-')); const destination=path.join(temp,'file');
    try {
        const entry={ kind:'file' as const, uid:'a', version: current ? 'revision-a' : 'revision-old', size:3, hash:null };
        await f.drive().download(entry, destination);
        expect(readFileSync(destination,'utf8')).toBe('abc');
        const mtime=statSync(destination).mtime.getTime();
        if (current) expect(mtime).toBe(then.getTime()); else expect(Math.abs(mtime-Date.now())).toBeLessThan(60_000);
    } finally { rmSync(temp,{recursive:true,force:true}); }
});

test('checks before a change use the event index instead of listing every folder on the path', async () => {
    const f=fixture(); f.nodes.set('docs',f.node('docs','Docs','root',true)); f.nodes.set('deep',f.node('deep','report.txt','docs'));
    const d=f.drive(); await d.snapshot('root'); const listings=f.listings();
    expect((await d.inspect('root','Docs/report.txt'))?.uid).toBe('deep');
    expect(await d.inspect('root','Docs/missing.txt')).toBeNull();
    expect(f.listings()).toBe(listings);
});
test('checks before a change see edits and trashing delivered by events', async () => {
    const f=fixture(); const d=f.drive(); await d.snapshot('root');
    const edited=f.node('a','a.txt','root'); edited.activeRevision!.uid='revision-new'; f.nodes.set('a',edited);
    f.events([{ type: 'node_updated', nodeUid: 'a', parentNodeUid: 'root', eventId: 'edit' }]);
    await new Promise(r=>setTimeout(r,2_100));
    expect((await d.inspect('root','a.txt'))?.version).toBe('revision-new');
    f.nodes.set('a',{...edited,trashTime:new Date()});
    f.events([{ type: 'node_updated', nodeUid: 'a', parentNodeUid: 'root', isTrashed: true, eventId: 'trash' }]);
    await new Promise(r=>setTimeout(r,2_100));
    expect(await d.inspect('root','a.txt')).toBeNull();
});
