// Devices authenticating with a company card (auth_activity.rs), for the Devices page.
// Kept up to date by the `authenticating-devices` event, which carries the whole list
// whenever it changes.
import { ref } from 'vue'
import { invoke, listen, type UnlistenFn } from 'src/backend'

export interface AuthenticatingDevice {
    imei: string
    company: string
    started_at: number // unix seconds
}

export const authenticatingDevices = ref<AuthenticatingDevice[]>([])

// Starts following the list. Call once (MainLayout); returns the listener to detach.
export const trackAuthenticatingDevices = async (): Promise<UnlistenFn> => {
    const unlisten = await listen<AuthenticatingDevice[]>('authenticating-devices', (event) => {
        authenticatingDevices.value = event.payload
    })
    authenticatingDevices.value = await invoke<AuthenticatingDevice[]>('get_authenticating_devices')
    return unlisten
}
