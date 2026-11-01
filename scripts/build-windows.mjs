import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { buildCatalog } from './build-catalog.mjs';
const root = path.resolve(import.meta.dirname, '..');
function run(program, args) { const result = spawnSync(program, args, { cwd: root, stdio: 'inherit', shell: false }); if (result.error) throw result.error; if (result.status !== 0) throw Error(`${program} exited with ${result.status}`); }
if (process.platform !== 'win32') throw Error('Windows packaging requires Windows. Frontend and core checks can run separately.');
buildCatalog(root);
run('cargo', ['build', '--release', '-p', 'workbench-core', '--locked']);
const args = ['build', '--config', 'src-tauri/tauri.bundle.conf.json'];
const certificate = process.env.WORKBENCH_CERTIFICATE_THUMBPRINT;
if (certificate) {
  if (!/^[a-f0-9]{40,64}$/i.test(certificate)) throw Error('Use the thumbprint of an installed code-signing certificate.');
  const config = path.join(root, '.workbench-data', 'signing.config.json'); fs.mkdirSync(path.dirname(config), { recursive: true });
  fs.writeFileSync(config, JSON.stringify({ bundle: { windows: { certificateThumbprint: certificate, digestAlgorithm: 'sha256', timestampUrl: 'http://timestamp.digicert.com' } } }));
  args.push('--config', config);
}
run(process.execPath, [path.join(root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'), ...args]);
if (certificate) {
  const version = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8')).version;
  const installer = path.join(root, 'target', 'release', 'bundle', 'nsis', `Workbench_${version}_x64-setup.exe`);
  const script = `$s=Get-AuthenticodeSignature -LiteralPath '${installer.replaceAll("'", "''")}'; if($s.Status -ne 'Valid'){throw 'Installer code signature is not valid'}`;
  run('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')]);
}
