import { readFileSync, writeFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
  console.error('Usage: node scripts/version.mjs <major.minor.patch>');
  process.exit(1);
}

function updateJson(filePath, updater) {
  const content = readFileSync(filePath, 'utf-8');
  const json = JSON.parse(content);
  updater(json);
  writeFileSync(filePath, JSON.stringify(json, null, 2) + '\n');
}

// package.json
updateJson(resolve(root, 'package.json'), (j) => { j.version = version; });

// src-tauri/tauri.conf.json
updateJson(resolve(root, 'src-tauri/tauri.conf.json'), (j) => { j.version = version; });

// src-tauri/Cargo.toml — [package] 섹션의 version만 교체
const cargoPath = resolve(root, 'src-tauri/Cargo.toml');
const cargo = readFileSync(cargoPath, 'utf-8');
// [package] 블록 안의 첫 번째 version 줄만 교체 (의존성 version 줄은 건드리지 않음)
const updated = cargo.replace(/^(version\s*=\s*)"[^"]*"/m, `$1"${version}"`);
writeFileSync(cargoPath, updated);

console.log(`✓ ${version} 으로 업데이트됨`);
console.log('  package.json');
console.log('  src-tauri/tauri.conf.json');
console.log('  src-tauri/Cargo.toml');
console.log('  (Cargo.lock 은 다음 빌드 시 자동 갱신)');
