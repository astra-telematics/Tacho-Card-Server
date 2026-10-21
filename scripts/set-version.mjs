// Sets the app version everywhere it's recorded, e.g. from a release tag in CI:
//
//   node scripts/set-version.mjs 1.2.3
//
// package.json (+ package-lock.json), src-tauri/tauri.conf.json, and src-tauri/Cargo.toml
// (+ Cargo.lock, so `cargo build --locked` still passes). Accepts a leading "v".
import { readFileSync, writeFileSync } from 'node:fs'

const version = (process.argv[2] ?? '').replace(/^v/, '')
// SemVer: MAJOR.MINOR.PATCH with an optional -pre-release and +build.
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/.test(version)) {
    console.error(`"${process.argv[2] ?? ''}" isn't a version like 1.2.3 or 1.2.3-rc.1`)
    process.exit(1)
}

// Each pattern captures the text before and after the version it replaces.
const files = {
    // The first "version" is the package's own.
    'package.json': [/("version"\s*:\s*")[^"]*(")/],
    // The root package appears twice: at the top, and as packages[""].
    'package-lock.json': [
        /("version"\s*:\s*")[^"]*(")/,
        /("packages"\s*:\s*\{\s*""\s*:\s*\{[^}]*?"version"\s*:\s*")[^"]*(")/,
    ],
    'src-tauri/tauri.conf.json': [/("version"\s*:\s*")[^"]*(")/],
    // [package] comes first, so the first `version = ` line is ours.
    'src-tauri/Cargo.toml': [/^(version\s*=\s*")[^"]*(")/m],
    'src-tauri/Cargo.lock': [/(name = "tacho-card-server"\r?\nversion = ")[^"]*(")/],
}

for (const [path, patterns] of Object.entries(files)) {
    let text = readFileSync(path, 'utf8')
    for (const pattern of patterns) {
        if (!pattern.test(text)) {
            console.error(`${path}: couldn't find the version to set (${pattern})`)
            process.exit(1)
        }
        text = text.replace(pattern, `$1${version}$2`)
    }
    writeFileSync(path, text)
    console.log(`${path}: ${version}`)
}
