import { beforeAll, expect, test } from 'bun:test';
import { CryptoProxy } from '@protontech/crypto';
import { Api as CryptoApi } from '@protontech/crypto/proxy/endpoint/api.ts';
import { DriveCrypto, OpenPGPCryptoWithCryptoProxy, type PrivateKey } from '@protontech/drive-sdk/crypto';

// File content goes through the same crypto calls as a real upload and download, offline.
const logger = { debug() {}, info() {}, warn() {}, error() {} };
const crypto = new DriveCrypto({ getLogger: () => logger, recordMetric() {} }, new OpenPGPCryptoWithCryptoProxy(CryptoProxy), {} as never);
let parent: PrivateKey;

beforeAll(async () => {
    CryptoApi.init({});
    CryptoProxy.setEndpoint(new CryptoApi(), endpoint => endpoint.clearKeyStore());
    parent = await CryptoProxy.generateKey({ userIDs: [{ name: 'parent' }], type: 'ecc', curve: 'ed25519Legacy' });
});

// Blocks are one encrypted-data packet (tag 18): version 2 is AEAD (SEIPDv2), version 1 is not.
function packetVersion(block: Uint8Array) {
    expect(block[0]).toBe(0xc0 | 18);
    const len = block[1]!;
    const header = len < 192 ? 2 : len < 224 ? 3 : len === 255 ? 6 : 2;
    return block[header];
}

for (const enableAead of [true, false]) test(`content round-trips with ${enableAead ? 'AEAD' : 'legacy'} node keys`, async () => {
    // Uploader (Peedrive since this SDK revision, or Proton's own apps) creates the node and content keys.
    const node = await crypto.generateKey([parent], parent, { enableAead });
    const content = await crypto.generateContentKey(node.decrypted.key);
    const data = new TextEncoder().encode('peedrive '.repeat(10_000));
    const { encryptedData } = await crypto.encryptBlock(data, node.decrypted.key, content.decrypted.contentKeyPacketSessionKey, parent);
    expect(packetVersion(encryptedData)).toBe(enableAead ? 2 : 1);

    // Downloader only has the armored key material from the API.
    const { key } = await crypto.decryptKey(node.encrypted.armoredKey, node.encrypted.armoredPassphrase,
        node.encrypted.armoredPassphraseSignature, [parent], [parent]);
    const { sessionKey } = await crypto.decryptAndVerifySessionKey(content.encrypted.base64ContentKeyPacket,
        content.encrypted.armoredContentKeyPacketSignature, key, [key]);
    expect(await crypto.decryptBlock(encryptedData, sessionKey)).toEqual(data);
});
