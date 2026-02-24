// normalize_lf.js -- convert all Rust/JS/HTML/CSS source files to LF line endings locally.
// Run before editing. Git will still use CRLF on checkout (via .gitattributes) but working
// copies will be clean LF so search strings in patch scripts always match.
'use strict';
const fs   = require('fs');
const path = require('path');

const EXTENSIONS = new Set(['.rs', '.toml', '.js', '.ts', '.html', '.css', '.json', '.py', '.md', '.txt', '.sql']);
const SKIP_DIRS  = new Set(['target', 'node_modules', '.git', 'dist', 'build']);

let converted = 0, skipped = 0;

function walk(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) walk(path.join(dir, entry.name));
    } else if (entry.isFile() && EXTENSIONS.has(path.extname(entry.name).toLowerCase())) {
      const full = path.join(dir, entry.name);
      const raw  = fs.readFileSync(full, 'utf8');
      if (raw.includes('\r\n') || raw.includes('\r')) {
        const lf = raw.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
        fs.writeFileSync(full, lf, 'utf8');
        console.log('  LF  ' + full.replace(process.cwd() + path.sep, ''));
        converted++;
      } else {
        skipped++;
      }
    }
  }
}

process.chdir(path.dirname(__filename));
console.log('Normalizing line endings to LF in: ' + process.cwd());
walk(process.cwd());
console.log('\nDone. Converted: ' + converted + '  Already LF: ' + skipped);
