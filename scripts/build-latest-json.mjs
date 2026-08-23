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
  // The .sig file `tauri signer sign` writes is ALREADY the exact string the
  // updater's `signature` field expects (its bytes, read as text, decode via
  // one base64 pass into the minisign "untrusted comment: ..." format). Do
  // NOT base64-encode it again here — that double-encodes it, so the
  // updater's single base64 decode yields the still-encoded text instead of
  // the real signature, and verification fails (confirmed against the
  // published v2.1.0 latest.json).
  return readFileSync(sigPath, 'utf-8').trim();
}

const files = readdirSync(bundleDir);
// Not an NSIS installer since specs/0005-wpf-installer.md — it's the WPF
// app in installer/WumaTracker.Setup — but still just a *-setup.exe file
// here, found the same way. MSI/WiX isn't built in CI anymore either (see
// main.yml), so this is the only Windows artifact now.
const setupExe = files.find((f) => f.endsWith('.exe'));

if (!setupExe) {
  console.error(`설치 프로그램 산출물을 찾지 못했습니다. dir=${bundleDir} files=${files.join(', ')}`);
  process.exit(1);
}

// windows-x86_64는 tauri updater 플러그인이 실제로 참조하는 키.
// -msi/-nsis는 기존 tauri-action 산출물과의 호환을 위해 함께 남겨둔다
// (키 이름은 그대로 유지 — 실제로는 더 이상 NSIS가 아니지만, 이 키를 참조하는
// 쪽에서 문자열 자체를 특별 취급하진 않으므로 이름을 바꿀 이유가 없다). MSI
// 자체는 이제 안 만들어서 -msi 키는 뺐다.
const manifest = {
  version,
  notes,
  pub_date: new Date().toISOString(),
  platforms: {
    'windows-x86_64': {
      signature: sigFor(setupExe),
      url: releaseUrl(setupExe),
    },
    'windows-x86_64-nsis': {
      signature: sigFor(setupExe),
      url: releaseUrl(setupExe),
    },
  },
};

writeFileSync(resolve(root, 'latest.json'), JSON.stringify(manifest, null, 2) + '\n');

console.log(`✓ latest.json 생성됨 (v${version})`);
console.log(`  installer: ${basename(setupExe)}`);
