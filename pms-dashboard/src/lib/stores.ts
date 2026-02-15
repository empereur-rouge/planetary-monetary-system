import { writable } from 'svelte/store';

export const adminToken = writable<string>(sessionStorage.getItem('pms_admin_token') || '');

export const nodeStatus = writable<any>(null);
export const nodeMetrics = writable<any>(null);
export const networkPeers = writable<any[]>([]);

adminToken.subscribe(value => {
    if (value) {
        sessionStorage.setItem('pms_admin_token', value);
    } else {
        sessionStorage.removeItem('pms_admin_token');
    }
});

// --- Ledger stores ---

export interface LedgerSummary {
    id: string;
    network_id: string;
    prefix: string;
    protocol_version: number;
    block_count: number;
    tip_limit?: number;
}

export const ledgerList = writable<LedgerSummary[]>([]);

export const selectedLedgerId = writable<string | null>(
    sessionStorage.getItem('pms_selected_ledger') || null
);

selectedLedgerId.subscribe(value => {
    if (value) {
        sessionStorage.setItem('pms_selected_ledger', value);
    } else {
        sessionStorage.removeItem('pms_selected_ledger');
    }
});

// --- Token stores ---

export interface TokenInfo {
    asset_id: string;
    symbol: string;
    name: string;
    decimals: number;
    max_supply?: string;
    creator: string;
    mint_authority: string;
}

export const tokenList = writable<TokenInfo[]>([]);
