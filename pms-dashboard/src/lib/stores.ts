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
