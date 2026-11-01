import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { spawnSync, execFileSync } from 'node:child_process';
import { zipSync } from 'fflate';
import { buildCatalog } from './build-catalog.mjs';
process.env.TZ = 'UTC';
const root = path.resolve(import.meta.dirname, '..'); const version = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8')).version;
const out = path.join(root, 'output', 'release'); fs.mkdirSync(out, { recursive: true });
if (!process.argv.includes('--skip-build') && !process.argv.includes('--hashes-only')) { const result = spawnSync(process.execPath, [path.join(root, 'scripts', 'build-windows.mjs')], { stdio: 'inherit' }); if (result.status !== 0) process.exit(result.status || 1); }
const catalogAssets = buildCatalog(root); const installerName = `Workbench_${version}_x64-setup.exe`; const portableName = `Workbench-${version}-windows-x64-portable.zip`; const sourceName = `Workbench-${version}-source.zip`;
const portable = {}; const options = { mtime: new Date('2026-11-01T00:00:00Z') };
function addTree(files, folder, prefix) { for (const entry of fs.readdirSync(folder, { withFileTypes: true })) { if (entry.isSymbolicLink()) throw Error(`Refusing to package symlink ${entry.name}`); const file = path.join(folder, entry.name); const name = `${prefix}/${entry.name}`; if (entry.isDirectory()) addTree(files, file, name); else if (entry.isFile()) files[name] = [fs.readFileSync(file), options]; } }
if (!process.argv.includes('--hashes-only')) {
  fs.copyFileSync(path.join(root, 'target', 'release', 'bundle', 'nsis', installerName), path.join(out, installerName));
  for (const file of ['Workbench.exe', 'workbench-cli.exe']) portable[`Workbench/${file}`] = [fs.readFileSync(path.join(root, 'target', 'release', file)), options];
  for (const file of ['LICENSE', 'THIRD_PARTY_NOTICES.txt', 'README.md']) portable[`Workbench/${file}`] = [fs.readFileSync(path.join(root, file)), options];
  addTree(portable, path.join(root, 'dist'), 'Workbench/ui'); addTree(portable, path.join(root, 'docs'), 'Workbench/docs');
  fs.writeFileSync(path.join(out, portableName), zipSync(portable, { level: 6 }));
  const source = {}; const inventory = execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], { cwd: root, encoding: 'utf8' }).split('\0').filter(Boolean);
  for (const file of new Set(inventory)) {
    if (file.startsWith('.frontend-craft/')) continue;
    const absolute = path.resolve(root, file); if (!absolute.startsWith(root + path.sep)) throw Error(`Invalid source inventory ${file}`);
    if (fs.lstatSync(absolute).isSymbolicLink()) throw Error(`Refusing to package source symlink ${file}`);
    source[`Workbench-source/${file.replaceAll('\\', '/')}`] = [fs.readFileSync(absolute), options];
  }
  fs.writeFileSync(path.join(out, sourceName), zipSync(source, { level: 6 }));
}
const assets = [installerName, portableName, sourceName, ...catalogAssets]; const update = path.join(out, 'workbench-update.json');
if (fs.existsSync(update)) { const envelope = JSON.parse(fs.readFileSync(update, 'utf8')); const payload = JSON.parse(Buffer.from(envelope.payload, 'base64').toString('utf8')); if (payload.version === version) assets.push('workbench-update.json'); }
const sums = assets.map(name => `${createHash('sha256').update(fs.readFileSync(path.join(out, name))).digest('hex')}  ${name}`).join('\n') + '\n';
fs.writeFileSync(path.join(out, 'SHA256SUMS.txt'), sums); console.log(sums);
