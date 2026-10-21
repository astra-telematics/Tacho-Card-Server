// Downloads in progress (download_activity.rs), shared by the header indicator and the
// Devices page. Kept up to date by the `downloads-active` event, which carries the whole
// list whenever it changes.
import { ref } from 'vue'
import { invoke, listen, type UnlistenFn } from 'src/backend'

export interface ActiveDownload {
    imei: string
    company: string
    started_at: number // unix seconds
    bytes: number // received so far; the total isn't known until it ends
    generation: number
}

export const activeDownloads = ref<ActiveDownload[]>([])

// Starts following the list. Call once (MainLayout); returns the listener to detach.
export const trackActiveDownloads = async (): Promise<UnlistenFn> => {
    const unlisten = await listen<ActiveDownload[]>('downloads-active', (event) => {
        activeDownloads.value = event.payload
    })
    activeDownloads.value = await invoke<ActiveDownload[]>('get_active_downloads')
    return unlisten
}

export const formatBytes = (bytes: number) =>
    bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(0)} KB` : `${(bytes / 1024 / 1024).toFixed(1)} MB`
