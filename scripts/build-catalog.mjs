import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { zipSync } from 'fflate';
process.env.TZ = 'UTC';
export function buildCatalog(root = path.resolve(import.meta.dirname, '..')) {
  const version = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8')).version;
  const output = path.join(root, 'output', 'release'); fs.mkdirSync(output, { recursive: true });
  const packages = path.join(root, 'catalog', 'plugins'); const entries = [];
  for (const name of fs.readdirSync(packages).sort()) {
    const source = path.join(packages, name); const manifest = JSON.parse(fs.readFileSync(path.join(source, 'plugin.json'), 'utf8'));
    if (manifest.version !== version) throw Error(`Catalog version mismatch: ${name}`);
    const files = {};
    function walk(folder, relative = '') {
      for (const entry of fs.readdirSync(folder, { withFileTypes: true })) {
        const relativePath = relative ? `${relative}/${entry.name}` : entry.name;
        if (entry.isSymbolicLink()) throw Error(`Plugin symlink is not distributable: ${relativePath}`);
        if (entry.isDirectory()) walk(path.join(folder, entry.name), relativePath);
        else if (entry.isFile()) files[relativePath] = [fs.readFileSync(path.join(folder, entry.name)), { mtime: new Date('2026-11-01T00:00:00Z') }];
      }
    }
    walk(source);
    const bytes = zipSync(files, { level: 6 }); const asset = `workbench-plugin-${name}-${version}.zip`;
    fs.writeFileSync(path.join(output, asset), bytes);
    entries.push({ name: manifest.name, description: manifest.description, repository: 'RobertIonutF/Workbench', tag: `v${version}`, asset, sha256: createHash('sha256').update(bytes).digest('hex') });
  }
  const file = path.join(root, 'catalog', 'registry.json'); const json = JSON.stringify({ version: 1, plugins: entries }, null, 2) + '\n';
  if (!fs.existsSync(file) || fs.readFileSync(file, 'utf8') !== json) fs.writeFileSync(file, json);
  return entries.map(entry => entry.asset);
}
if (path.resolve(process.argv[1] || '') === import.meta.filename) console.log(`Built ${buildCatalog().length} digest-pinned starter plugins.`);
