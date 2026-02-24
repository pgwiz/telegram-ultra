const fs = require('fs');

function readFile(p) {
  return fs.readFileSync(p, 'utf8').replace(/\r\n/g, '\n').replace(/\r/g, '\n');
}

function writeFile(p, c) {
  fs.writeFileSync(p, c.replace(/\n/g, '\r\n'), 'utf8');
}

const PLAYLIST_DL = "./worker/playlist_dl.py";
let content = readFile(PLAYLIST_DL);

// Add format constants
if (!content.includes('AUDIO_FORMAT')) {
  console.log('Adding format constants...');
  const insert = 'logger = logging.getLogger(__name__)';
  const idx = content.indexOf(insert);
  const constants = '\n\nAUDIO_FORMAT = "bestaudio[ext=m4a]/bestaudio[ext=webm]/bestaudio/best"\nVIDEO_FORMAT = (\n    "bestvideo[height<=1080][ext=mp4]+bestaudio[ext=m4a]"\n    "/bestvideo[height<=1080]+bestaudio"\n    "/best[height<=1080]/best"\n)\n';
  content = content.slice(0, idx + insert.length) + constants + content.slice(idx + insert.length);
  writeFile(PLAYLIST_DL, content);
}

// Add archive support
if (!content.includes('--download-archive')) {
  console.log('Adding archive support...');
  content = readFile(PLAYLIST_DL);
  const search = "        format_str = params.get('format', 'bestaudio[ext=m4a]/bestaudio')\n        command.extend(['-f', format_str])";
  const replace = "        if extract_audio:\n            format_str = params.get('format', AUDIO_FORMAT)\n        else:\n            format_str = params.get('format', VIDEO_FORMAT)\n        command.extend(['-f', format_str])\n\n        archive_path = params.get('archive_file')\n        if archive_path:\n            safe_mkdir(os.path.dirname(archive_path))\n            command.extend(['--download-archive', archive_path])";
  if (content.includes(search)) {
    content = content.replace(search, replace);
    writeFile(PLAYLIST_DL, content);
  }
}

// Create playlist_utils.py
if (!fs.existsSync('./worker/playlist_utils.py')) {
  console.log('Creating playlist_utils.py...');
  fs.writeFileSync('./worker/playlist_utils.py', fs.readFileSync(__filename, 'utf8').split('__EOF__')[1].replace(/\r\n/g, '\r\n'), 'utf8');
}

console.log('Done!');
__EOF__
node update_playlist.js
