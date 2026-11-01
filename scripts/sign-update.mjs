import fs from 'node:fs';
import path from 'node:path';
import { createHash, createPrivateKey, createPublicKey, sign } from 'node:crypto';
import { spawnSync } from 'node:child_process';
const root = path.resolve(import.meta.dirname, '..');
const version = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8')).version;
const output = path.join(root, 'output', 'release'); const installer = `Workbench_${version}_x64-setup.exe`;
let pem = process.env.WORKBENCH_UPDATE_PRIVATE_KEY_PEM;
if (!pem && process.platform === 'win32') {
  const protectedFile = path.join(process.env.LOCALAPPDATA || '', 'WorkbenchPublisher', 'update-private.dpapi');
  if (!fs.existsSync(protectedFile)) throw Error('Configure a publisher signing key; no private key is bundled with the project.');
  const script = `Add-Type -AssemblyName System.Security; [Convert]::ToBase64String([System.Security.Cryptography.ProtectedData]::Unprotect([System.IO.File]::ReadAllBytes('${protectedFile.replaceAll("'", "''")}'),$null,[System.Security.Cryptography.DataProtectionScope]::CurrentUser))`;
  const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')], { encoding: 'utf8' });
  if (result.status !== 0) throw Error('The publisher key could not be decrypted by this Windows user.');
  pem = Buffer.from(result.stdout.trim(), 'base64').toString('utf8');
}
if (!pem) throw Error('Set WORKBENCH_UPDATE_PRIVATE_KEY_PEM to the publisher Ed25519 PKCS8 PEM key.');
const privateKey = createPrivateKey(pem); const publicKey = createPublicKey(privateKey).export({ type: 'spki', format: 'der' }).subarray(-32).toString('base64');
if (publicKey !== fs.readFileSync(path.join(root, '.github', 'workbench-update-key.pub'), 'utf8').trim()) throw Error('Publisher key does not match this app’s trusted public key.');
const channel = process.argv.includes('--stable') ? 'stable' : 'preview';
const notes = fs.existsSync(path.join(root, 'CHANGELOG.md')) ? fs.readFileSync(path.join(root, 'CHANGELOG.md'), 'utf8') : `Workbench ${version}`;
const payload = { version, platform: 'windows-x86_64', channel, url: `https://github.com/${process.env.WORKBENCH_REPOSITORY || 'RobertIonutF/Workbench'}/releases/download/v${version}/${installer}`, sha256: createHash('sha256').update(fs.readFileSync(path.join(output, installer))).digest('hex'), releaseNotes: notes };
const bytes = Buffer.from(JSON.stringify(payload));
const envelope = { payload: bytes.toString('base64'), signature: sign(null, bytes, privateKey).toString('base64') };
fs.writeFileSync(path.join(output, 'workbench-update.json'), JSON.stringify(envelope, null, 2) + '\n');
console.log(`Signed ${version} ${channel} update manifest; private key was not written to release files.`);
