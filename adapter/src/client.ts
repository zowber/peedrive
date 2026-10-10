import { mkdir, chmod } from 'node:fs/promises';
import path from 'node:path';
import { CryptoProxy } from '@protontech/crypto';
import { Api as CryptoApi } from '@protontech/crypto/proxy/endpoint/api.ts';
import { computeKeyPassword, generateKeySalt, getRandomSrpVerifier, getSrp } from '@protontech/crypto/srp';
import { OpenPGPCryptoWithCryptoProxy, ProtonDriveClient } from '@protontech/drive-sdk';
import { ApiClient, initAccount } from 'proton-drive-sdk-account';
import { Credentials } from '../../vendor/proton-sdk/cli/src/credentials/credentials';
import type { CredentialsStore, Credentials as StoredCredentials } from '../../vendor/proton-sdk/cli/src/credentials/interface';
import { parseStoredSnapshot } from '../../vendor/proton-sdk/cli/src/credentials/parseCredentials';
import { getConfig } from '../../vendor/proton-sdk/cli/src/config';
import { createCaches } from '../../vendor/proton-sdk/cli/src/cache';
import { EncryptedSQLiteCache } from '../../vendor/proton-sdk/cli/src/cache/encryptedCache';
import { DriveAccountAdapter } from '../../vendor/proton-sdk/cli/src/api/driveAccountAdapter';
import { HTTPClient } from '../../vendor/proton-sdk/cli/src/api/httpClient';
import { SIGNED_OUT } from './errors';
import { processDriveRequirementHeaders, SUPPORTED_REQUIREMENT_MASK_BY_SCOPE } from '../../vendor/proton-sdk/cli/src/api/apiRequirements';

// Nothing reaches telemetry or stdout. Warnings go to stderr (the user journal) without
// payloads: status codes and Proton error codes only, so session loss can be diagnosed.
function describe(detail: unknown): string {
    if (detail instanceof Response) {
        void detail.clone().json().then((body: { Code?: number; Error?: string }) =>
            console.error(`peedrive-adapter:   HTTP ${detail.status} Proton code ${body.Code ?? '?'}: ${body.Error ?? ''}`), () => {});
        return ` (HTTP ${detail.status})`;
    }
    return detail instanceof Error ? ` (${detail.name}: ${detail.message})` : '';
}
const logger = { debug(_m: string) {}, info(_m: string) {},
    warn(m: string, d?: unknown) { console.error(`peedrive-adapter: warning: ${m}${describe(d)}`); },
    error(m: string, d?: unknown) { console.error(`peedrive-adapter: error: ${m}${describe(d)}`); } };
type Secrets = Pick<typeof Bun.secrets, 'get' | 'set' | 'delete'>;
const SESSION = { service: 'io.github.zowber.Peedrive', name: 'account-session-v1' };
/** Releases up to 0.1.0~alpha4 used this app ID; their session moves across on first load. */
const LEGACY_SESSION = { service: 'io.github.andybright.Peedrive', name: 'account-session-v1' };
export class SecretStore implements CredentialsStore {
    constructor(private secrets: Secrets = Bun.secrets) {}
    async load(): Promise<StoredCredentials | null> {
        let raw = await this.secrets.get(SESSION);
        if (raw === null) {
            raw = await this.secrets.get(LEGACY_SESSION);
            // Copy, verify, then delete, so an interrupted move never loses the session.
            if (raw !== null) {
                await this.secrets.set({ ...SESSION, value: raw });
                if (await this.secrets.get(SESSION) === raw) await this.secrets.delete(LEGACY_SESSION);
            }
        }
        return parseStoredSnapshot(raw);
    }
    async save(value: StoredCredentials): Promise<void> {
        // Refresh tokens rotate. A lost write signs the account out on the next start, so verify it.
        const serialized = JSON.stringify(value);
        await this.secrets.set({ ...SESSION, value: serialized });
        if (await this.secrets.get(SESSION) !== serialized) throw new Error('GNOME Keyring did not store the refreshed session');
    }
    async remove(): Promise<void> {
        console.error('peedrive-adapter: removing the saved Proton session');
        await this.secrets.delete(SESSION);
        await this.secrets.delete(LEGACY_SESSION);
    }
}

// Set per build by scripts/build-adapter.sh; unbuilt development runs say so.
declare const PEEDRIVE_APP_VERSION: string | undefined;
export const APP_VERSION = typeof PEEDRIVE_APP_VERSION === 'string' ? PEEDRIVE_APP_VERSION : 'external-drive-peedrive@0.1.0-alpha+dev';

export async function createClient(dataDir: string) {
    await mkdir(dataDir, { recursive: true, mode: 0o700 }); await chmod(dataDir, 0o700);
    const config = { ...getConfig({ appVersion: APP_VERSION, sdkVersion: '0.0.1', clientUidPrefix: 'peedrive' }),
        baseUrl: 'drive-api.proton.me', accountUrl: 'account.proton.me', authClientId: 'external-drive',
        cacheDir: dataDir, appDir: dataDir, logDir: dataDir, unsafeCache: false };
    CryptoApi.init({});
    CryptoProxy.setEndpoint(new CryptoApi(), endpoint => endpoint.clearKeyStore());
    const credentials = new Credentials(new SecretStore(), logger);
    let updateRequired: string | undefined;
    const apiClient = new ApiClient({ baseUrl: config.baseUrl, appVersion: config.appVersion, credentials, logger,
        headers: { 'x-pm-drive-sdk-version': '0.0.1+8ffae00' },
        afterResponseHooks: [(_request, _options, response) => {
            processDriveRequirementHeaders(response.headers, {
                clientSdkVersion: '0.0.1', supportedRequirementMasksByScope: SUPPORTED_REQUIREMENT_MASK_BY_SCOPE,
                onUnsupportedFeature: () => { updateRequired = 'Proton Drive requires a newer client. Update Peedrive before syncing.'; },
                onRequiredUpdate: () => { updateRequired = 'Proton Drive requires a newer SDK. Update Peedrive before syncing.'; },
                onSuggestedUpdate: () => {},
            });
            if (updateRequired) throw new Error(updateRequired);
        }],
    });
    const account = await initAccount({ authClientId: config.authClientId, apiClient, credentials, cryptoProxy: CryptoProxy, logger, accountUrl: config.accountUrl,
        srpApi: { computeKeyPassword, generateKeySalt, getRandomSrpVerifier, getSrp } });
    const caches = createCaches(config, credentials, logger);
    const sdk = new ProtonDriveClient({ config: { baseUrl: config.baseUrl }, httpClient: new HTTPClient(apiClient),
        ...caches, account: new DriveAccountAdapter(account.addresses), srpModule: account.srp,
        openPGPCryptoModule: new OpenPGPCryptoWithCryptoProxy(CryptoProxy),
        telemetry: { getLogger: () => logger, recordMetric: () => {} },
    });
    const index = new EncryptedSQLiteCache(path.join(dataDir, 'remote-index.sqlite'), () => credentials.getCachePassword(), logger);
    return { sdk, auth: account.auth, addresses: account.addresses, accountApi: account.accountApi, credentials, caches, index,
        assertReady() { if (updateRequired) throw new Error(updateRequired); if (!account.auth.isLoggedIn()) throw new Error(SIGNED_OUT); } };
}
export type Client = Awaited<ReturnType<typeof createClient>>;
