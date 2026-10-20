// Talks to the Rust backend: through Tauri in the desktop app, or over HTTP to the
// headless server's web UI (src-tauri/src/web_server.rs) in a browser. Same `invoke`,
// `listen` and `emit` as @tauri-apps/api, so components don't care which.
import { ref } from 'vue'
import { invoke as tauriInvoke } from '@tauri-apps/api/core'
import { emit as tauriEmit, listen as tauriListen } from '@tauri-apps/api/event'

// True in the desktop app. Desktop-only features (tray, open logs folder, folder picker)
// are hidden when false.
export const isDesktop = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

export type UnlistenFn = () => void
export interface BackendEvent<T> {
    payload: T
}

// Who's logged in. Always admin on the desktop, which has no login.
export const currentUser = ref<string | null>(isDesktop ? 'admin' : null)

// Called when a request finds the session gone (logged out elsewhere, expired, server
// restarted). Set by the router to go to the login page.
let loggedOutHandler: (() => void) | null = null
export const onLoggedOut = (handler: () => void) => {
    loggedOutHandler = handler
}

const loggedOut = () => {
    currentUser.value = null
    closeEvents()
    loggedOutHandler?.()
}

// POSTs JSON; resolves with the JSON reply or throws it (errors are a string, as from
// Tauri). A 401 from anything but login means the session's gone.
const post = async (url: string, body: unknown, isLogin = false): Promise<unknown> => {
    const response = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body ?? {}),
    })
    if (response.status === 401 && !isLogin) loggedOut()
    const text = await response.text()
    let value: unknown = null
    try {
        value = text ? JSON.parse(text) : null
    } catch {
        value = text
    }
    // A string, like Tauri's command errors, which is what the pages expect.
    // eslint-disable-next-line @typescript-eslint/only-throw-error
    if (!response.ok) throw value ?? `Request failed (${response.status})`
    return value
}

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    if (isDesktop) return tauriInvoke<T>(command, args)
    return (await post(`/api/invoke/${command}`, args)) as T
}

// ---- events: one shared EventSource for all listeners in a browser ----

type Handler = (event: BackendEvent<unknown>) => void
const handlers = new Map<string, Set<Handler>>()
let source: EventSource | null = null
let opened: Promise<void> | null = null

const attach = (name: string) => {
    source?.addEventListener(name, (event) => {
        const payload: unknown = JSON.parse((event as MessageEvent<string>).data)
        handlers.get(name)?.forEach((handler) => handler({ payload }))
    })
}

const openEvents = () => {
    if (opened) return opened
    const events = new EventSource('/api/events')
    source = events
    for (const name of handlers.keys()) attach(name)
    let reconnecting = false
    opened = new Promise<void>((resolve) => {
        events.addEventListener('open', () => {
            resolve()
            // Events sent while disconnected are lost; ask for current state again.
            if (reconnecting) void invoke('resend_state').catch(() => {})
        })
        events.addEventListener('error', () => {
            resolve()
            reconnecting = true
            // EventSource reconnects by itself, except after an error status such as
            // 401. Then check whether we're still logged in.
            if (events.readyState === EventSource.CLOSED && source === events) {
                void getSession().then((user) => {
                    if (user === null) loggedOut()
                    else if (source === events) {
                        closeEvents()
                        setTimeout(() => void openEvents(), 5000)
                    }
                })
            }
        })
    })
    return opened
}

const closeEvents = () => {
    source?.close()
    source = null
    opened = null
}

export async function listen<T>(
    name: string,
    handler: (event: BackendEvent<T>) => void,
): Promise<UnlistenFn> {
    if (isDesktop) return tauriListen<T>(name, handler)
    let set = handlers.get(name)
    if (!set) {
        set = new Set()
        handlers.set(name, set)
        attach(name)
    }
    const registered = handler as Handler
    set.add(registered)
    await openEvents()
    return () => {
        set.delete(registered)
    }
}

export async function emit(name: string, payload?: unknown): Promise<void> {
    if (isDesktop) return tauriEmit(name, payload)
    // The only event the frontend sends: a page has loaded and wants current state.
    if (name === 'frontend-loaded') await invoke('resend_state')
}

// ---- login (browser only) ----

// The logged-in username, or null.
export const getSession = async (): Promise<string | null> => {
    if (isDesktop) return currentUser.value
    try {
        const response = await fetch('/api/session')
        const user = response.ok ? ((await response.json()) as { username: string }).username : null
        currentUser.value = user
        return user
    } catch {
        return null
    }
}

// Throws the error message (a string) if the login is refused.
export const login = async (username: string, password: string) => {
    const result = (await post('/api/login', { username, password }, true)) as { username: string }
    currentUser.value = result.username
}

export const logout = async () => {
    try {
        await post('/api/logout', {}, true)
    } finally {
        loggedOut()
    }
}
