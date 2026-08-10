/// <reference types="node" />
export type DocumentInput = { type: 'bytes'; data: Uint8Array; fileName: string }
export interface ConvertOptions { ocrLanguage?: string; enableVision?: boolean; ffmpegPath?: string }
export interface PaddleOcrConfig { endpoint: string; headers?: Record<string, string>; timeoutMs?: number; retries?: number; extraParams?: Record<string, unknown> }
export interface ConversionOptions { convert?: ConvertOptions; paddleOcr?: PaddleOcrConfig }
export interface ConvertResult { markdown: string; title: string; source: Record<string, unknown>; sections: unknown[]; assets: unknown[]; metadata: Record<string, unknown>; decisions: unknown[]; warnings: string[]; durationMs: number }
export function convert(input: DocumentInput, options?: ConversionOptions): Promise<ConvertResult>
export function getSupportedFormats(): Array<{category: string; extensions: string[]; requiresProvider: boolean}>

