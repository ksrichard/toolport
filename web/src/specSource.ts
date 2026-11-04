// Where a new API spec comes from: a URL, a file, or (on the APIs page) a template.

import { useState } from 'react'
import type { ImportApi } from './types/ImportApi'

type Mode = 'url' | 'file' | 'template'

export interface SpecFile {
  name: string
  text: string
}

export function useSpecSource() {
  const [mode, setMode] = useState<Mode>('url')
  const [url, setUrl] = useState('')
  const [file, setFile] = useState<SpecFile | null>(null)
  const [template, setTemplate] = useState('')
  const [baseUrl, setBaseUrl] = useState('')

  const ready = mode === 'url' ? url.trim() !== '' : mode === 'file' ? file !== null : template !== ''
  const source = (): ImportApi => {
    if (mode === 'template') return { source: 'template', template }
    const base = baseUrl.trim() || undefined
    return mode === 'url' ? { source: 'url', spec_url: url.trim(), base_url: base } : { source: 'text', spec: file?.text ?? '', base_url: base }
  }
  return { mode, setMode, url, setUrl, file, setFile, template, setTemplate, baseUrl, setBaseUrl, ready, source }
}

export type SpecSource = ReturnType<typeof useSpecSource>

/** Reads a picked file as text. Specs are JSON, so the text is all the server needs. */
export async function readSpecFile(input: HTMLInputElement): Promise<SpecFile | null> {
  const file = input.files?.[0]
  return file ? { name: file.name, text: await file.text() } : null
}

