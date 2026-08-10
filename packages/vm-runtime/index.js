'use strict'

const native = require('./vento_vm_runtime.node')

class RuntimeClient {
  constructor(options) { this.baseUrl = options.baseUrl; this.token = options.token }
  request(method, path, body) { return native.requestRuntime(this.baseUrl, this.token, method, path, body === undefined ? undefined : JSON.stringify(body)).then(value => value ? JSON.parse(value) : undefined) }
  create(options) { return this.request('POST', '/sandboxes', options).then(info => new Sandbox(this, info)) }
  connect(id) { return this.request('GET', `/sandboxes/${encodeURIComponent(id)}`).then(info => new Sandbox(this, info)) }
}
class Sandbox {
  constructor(client, info) { this.client = client; this.info = info; this.sandboxId = info.sandboxId }
  get commands() { return { run: request => this.client.request('POST', `/sandboxes/${this.sandboxId}/commands`, Array.isArray(request) ? {command: request} : request) } }
  get files() { return {
    write: (path, data) => native.requestRuntimeBinary(this.client.baseUrl, this.client.token, 'PUT', `/sandboxes/${this.sandboxId}/files/content?path=${encodeURIComponent(path)}`, data).then(() => undefined),
    read: path => native.requestRuntimeBinary(this.client.baseUrl, this.client.token, 'GET', `/sandboxes/${this.sandboxId}/files/content?path=${encodeURIComponent(path)}`, undefined),
    list: path => this.client.request('GET', `/sandboxes/${this.sandboxId}/files?path=${encodeURIComponent(path)}`),
  } }
  pause() { return this.client.request('POST', `/sandboxes/${this.sandboxId}/pause`) }
  resume() { return this.client.request('POST', `/sandboxes/${this.sandboxId}/resume`) }
  destroy() { return this.client.request('DELETE', `/sandboxes/${this.sandboxId}`) }
}
exports.RuntimeClient = RuntimeClient
exports.Sandbox = Sandbox
exports.startLocalRuntime = native.startLocalRuntime
