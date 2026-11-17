// Interfaces
export interface SmartCard {
    name?: string
    iccid?: string
    expire?: number | null
    card_type?: number | null
    structure_version?: [number, number] | null
    company_name?: string | null
    company_address?: string | null
    last_auth?: [number, boolean] | null
    company?: string | null // id of the TCS company it's assigned to; none = unassigned
}

export interface Reader {
    name: string
    status: string
    iccid: string
    card_number: string
    online?: boolean | undefined
    authentication?: boolean | undefined
}

// Companies, their cards and devices (see config.rs: get_companies / get_devices).

export interface CardEntry extends SmartCard {
    card_number: string
    company?: string | null
}

export interface Company {
    id: string
    name: string
    connection_limit: number
    download_interval_days: number
    card_download_interval_days: number
    devices: string[]
    cards: CardEntry[]
}

export interface CompaniesView {
    companies: Company[]
    unassigned_cards: CardEntry[]
}

export interface RegisteredDevice {
    imei: string
    company: string
    vrn: string | null // vehicle registration from the latest verified download
    last_auth_at: number | null // unix seconds of the latest authentication attempt
    last_auth_succeeded: boolean | null
    last_download_at: number | null // unix seconds of the latest verified download
}

export interface UnknownDevice {
    imei: string
    first_seen: number // unix seconds
    last_seen: number // unix seconds
    attempts: number
    firmware_version: string | null
    vrn: string | null // if it has downloaded before
}

export interface DevicesView {
    sole_company: string | null
    registered: RegisteredDevice[]
    unknown: UnknownDevice[]
}

// A driver card's latest verified download (config.rs, get_driver_cards). Dates are
// server-local YYYY-MM-DD; last_download_at is Unix seconds.
export interface DriverCard {
    company: string
    card_number: string
    last_download_at: number
    last_download_date: string
    imei: string
    vrn: string | null
    // 1 = driver slot, 2 = co-driver slot.
    slot: number
    downloads: number
    // When it's due again by the company's card download interval (null: no limit).
    next_due_date: string | null
    // When regulations need it downloaded again by.
    legal_due_date: string
}

export interface DriverCardsView {
    today: string
    cards: DriverCard[]
}

// A user account (users.rs, get_users). Times are Unix seconds.
export interface UserAccount {
    username: string
    created_at: number
    updated_at: number
}
