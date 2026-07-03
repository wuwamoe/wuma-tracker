import { readFileSync, writeFileSync, readdirSync } from 'fs';
import { resolve, dirname, basename } from 'path';
import { fileURLToPath } from 'url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const REPO = 'wuwamoe/wuma-tracker';

const bundleDir = process.argv[2];
if (!bundleDir) {
  console.error('Usage: node scripts/build-latest-json.mjs <windows-bundle-dir>');
  process.exit(1);
}

const { version } = JSON.parse(readFileSync(resolve(root, 'src-tauri/tauri.conf.json'), 'utf-8'));
const notes = readFileSync(resolve(root, '.github/release-notes.md'), 'utf-8').trimEnd();

const releaseUrl = (filename) =>
  `https://github.com/${REPO}/releases/download/v${version}/${filename}`;

function sigFor(filename) {
  const sigPath = resolve(bundleDir, `${filename}.sig`);
  return readFileSync(sigPath).toString('base64');
}

const files = readdirSync(bundleDir);
const msi = files.find((f) => f.endsWith('.msi'));
const nsis = files.find((f) => f.endsWith('.exe'));

if (!msi || !nsis) {
  console.error(`msi/nsis 산출물을 찾지 못했습니다. dir=${bundleDir} files=${files.join(', ')}`);
  process.exit(1);
}

// windows-x86_64는 tauri updater 플러그인이 실제로 참조하는 키.
// -msi/-nsis는 기존 tauri-action 산출물과의 호환을 위해 함께 남겨둔다.
const manifest = {
  version,
  notes,
  pub_date: new Date().toISOString(),
  platforms: {
    'windows-x86_64': {
      signature: sigFor(msi),
      url: releaseUrl(msi),
    },
    'windows-x86_64-msi': {
      signature: sigFor(msi),
      url: releaseUrl(msi),
    },
    'windows-x86_64-nsis': {
      signature: sigFor(nsis),
      url: releaseUrl(nsis),
    },
  },
};

writeFileSync(resolve(root, 'latest.json'), JSON.stringify(manifest, null, 2) + '\n');

console.log(`✓ latest.json 생성됨 (v${version})`);
console.log(`  msi:  ${basename(msi)}`);
console.log(`  nsis: ${basename(nsis)}`);
