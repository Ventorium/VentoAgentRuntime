const fs = require('fs');
const path = require('path');
const { convertBytes } = require('./index.js');

const dir = path.resolve(__dirname, '../../test-files');
const outDir = path.resolve(__dirname, '../../test-files/out');
fs.mkdirSync(outDir, { recursive: true });

// Embedded-image OCR routes to the remote provider; without credentials the
// images degrade to plain `![图片N]()` markers, and with them each image is
// followed by a quoted `> 图片N的OCR解析结果如下：` block. Token comes from
// PADDLE_OCR_TOKEN, never hardcoded.
const ocrToken = process.env.PADDLE_OCR_TOKEN || '';
const options = JSON.stringify({
  paddleOcr: {
    endpoint: process.env.PADDLE_OCR_URL || 'https://paddleocr.aistudio-app.com/api/v2/ocr/jobs',
    headers: ocrToken ? { Authorization: `bearer ${ocrToken}` } : {},
    model: process.env.PADDLE_OCR_VERSION || 'PaddleOCR-VL-1.6',
  },
});

(async () => {
  for (const f of fs
    .readdirSync(dir)
    .filter((f) => !f.startsWith('.') && fs.statSync(path.join(dir, f)).isFile())) {
    const data = fs.readFileSync(path.join(dir, f));
    console.log(`converting ${f}...`);
    try {
      // The binding now returns the markdown string directly.
      const markdown = await convertBytes(data, f, options);
      const base = f.replace(/\.[^.]+$/, '');
      const mdPath = path.join(outDir, `${base}.md`);
      fs.writeFileSync(mdPath, markdown, 'utf8');
      console.log(`${f} -> ${mdPath} (${markdown.length} chars)`);
    } catch (e) {
      console.log(`${f} -> ERROR: ${e.message}`);
    }
  }
})();
