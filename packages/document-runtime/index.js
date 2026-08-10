'use strict'

const native = require('./vento_document_runtime.node')

exports.convert = async function convert(input, options) {
  if (input.type !== 'bytes') throw new TypeError('The npm MVP accepts bytes input; use the Rust API for path and URL inputs')
  const raw = await native.convertBytes(input.data, input.fileName, options ? JSON.stringify(options) : undefined)
  return JSON.parse(raw)
}
exports.getSupportedFormats = function getSupportedFormats() {
  return JSON.parse(native.supportedFormats())
}

