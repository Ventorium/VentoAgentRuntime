const fs = require('fs');
const path = require('path');
const { convertBytes } = require('./index.js');

const dir = path.resolve(__dirname, '../../test-files');
const outDir = path.resolve(__dirname, '../../test-files/out');
fs.mkdirSync(outDir, { recursive: true });

// Images route to remote OCR; without a provider they fail with OCR_REQUIRED.
// Token comes from PADDLE_OCR_TOKEN, falling back to the dev token in test-ocr.py.
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
      const result = JSON.parse(await convertBytes(data, f, options));
      const base = f.replace(/\.[^.]+$/, '');
      const mdPath = path.join(outDir, `${base}.md`);
      fs.writeFileSync(mdPath, result.markdown, 'utf8');
      // Embedded images: write next to the markdown so marker URLs resolve.
      for (const img of result.images || []) {
        fs.writeFileSync(path.join(outDir, img.name), Buffer.from(img.dataBase64, 'base64'));
      }
      fs.writeFileSync(
        path.join(outDir, `${base}.meta.json`),
        JSON.stringify(
          {
            title: result.title,
            source: result.source,
            decisions: result.decisions,
            warnings: result.warnings,
            images: (result.images || []).map((img) => img.name),
            durationMs: result.durationMs,
          },
          null,
          2,
        ),
        'utf8',
      );
      const imgCount = (result.images || []).length;
      console.log(`${f} -> ${mdPath} (${result.markdown.length} chars, ${imgCount} images, ${result.durationMs}ms)`);
    } catch (e) {
      console.log(`${f} -> ERROR: ${e.message}`);
    }
  }
})();
