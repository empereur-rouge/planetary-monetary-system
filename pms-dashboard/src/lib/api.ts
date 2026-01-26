import { get } from 'svelte/store';
import { adminToken } from './stores';

const API_BASE = import.meta.env.VITE_API_BASE || '';

export async function apiCall(endpoint: string, method = 'GET', body?: any) {
    const token = get(adminToken);
    console.log(`Making ${method} request to ${endpoint} with token: ${token ? 'PRESENT' : 'MISSING'}`);

    const headers: HeadersInit = {
        'Content-Type': 'application/json',
    };

    if (token) {
        headers['Authorization'] = `Bearer ${token}`;
    }

    try {
        const res = await fetch(`${API_BASE}${endpoint}`, {
            method,
            headers,
            body: body ? JSON.stringify(body) : undefined,
        });

        if (res.status === 401 || res.status === 403) {
            console.warn('Unauthorized access');
            // Could redirect to login or clear token if needed
            // adminToken.set(''); 
            throw new Error('Unauthorized');
        }

        if (!res.ok) {
            const txt = await res.text();
            throw new Error(`API Error: ${res.status} ${txt}`);
        }

        // Handle text responses appropriately
        const contentType = res.headers.get('content-type');
        if (contentType && contentType.includes('application/json')) {
            return await res.json();
        }
        return await res.text();

    } catch (err) {
        console.error('API Call failed:', err);
        throw err;
    }
}
