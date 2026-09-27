/**
 * 剪贴板粘贴与拖入图片的解析测试。
 *
 * 覆盖方案 §4.2 要求的「粘贴代码块/图片、拖入图片」验收：
 * - 从资源管理器拖入/复制的图片文件被接受；
 * - 截图工具写入剪贴板的位图（只有 MIME、没有文件名）被接受并合成可读名字；
 * - 非图片文件、不支持的图片格式给出明确拒绝原因，而不是静默忽略；
 * - 与图片无关的粘贴保持编辑器原生行为（返回 none）。
 */
import { describe, expect, it } from 'vitest'
import {
  dragHasFiles,
  extractImage,
  isAcceptedFileName,
  isAcceptedMime,
  pastedFileName,
} from '@/services/imageDrop'

/** 最小合法 PNG 字节。 */
const PNG_BYTES = Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3])

/** 构造一个 File。 */
function makeFile(name: string, type: string, bytes: Uint8Array = PNG_BYTES): File {
  // `BlobPart` 要求底层是 `ArrayBuffer`（而非 `SharedArrayBuffer`），
  // 因此显式复制到一个独立缓冲区。
  const buffer = new ArrayBuffer(bytes.byteLength)
  new Uint8Array(buffer).set(bytes)
  return new File([buffer], name, { type })
}

/** 构造一个只含 files 的 DataTransfer 替身。 */
function dataWithFiles(files: File[]): DataTransfer {
  return {
    files,
    items: files.map((file) => ({
      kind: 'file',
      type: file.type,
      getAsFile: () => file,
    })),
    types: ['Files'],
  } as unknown as DataTransfer
}

/** 构造一个只有位图 item（无文件名）的剪贴板 DataTransfer 替身。 */
function dataWithBitmap(type: string, bytes: Uint8Array = PNG_BYTES): DataTransfer {
  const file = makeFile('', type, bytes)
  return {
    files: [],
    items: [{ kind: 'file', type, getAsFile: () => file }],
    types: ['Files'],
  } as unknown as DataTransfer
}

describe('接受判定', () => {
  it('按扩展名接受四种支持的位图格式', () => {
    for (const name of ['a.png', 'b.JPG', 'c.jpeg', 'd.webp', 'e.gif']) {
      expect(isAcceptedFileName(name), name).toBe(true)
    }
    expect(isAcceptedFileName('a.svg')).toBe(false)
    expect(isAcceptedFileName('a.html')).toBe(false)
    expect(isAcceptedFileName('a.md')).toBe(false)
  })

  it('按 MIME 接受四种支持的位图格式', () => {
    for (const mime of ['image/png', 'image/jpeg', 'image/webp', 'image/gif']) {
      expect(isAcceptedMime(mime), mime).toBe(true)
    }
    // SVG 与其它矢量/文档格式不接受（后端也不认这些文件头）。
    expect(isAcceptedMime('image/svg+xml')).toBe(false)
    expect(isAcceptedMime('text/html')).toBe(false)
    expect(isAcceptedMime('application/pdf')).toBe(false)
  })

  it('为剪贴板位图合成可读文件名与正确扩展名', () => {
    expect(pastedFileName('image/png')).toBe('粘贴的图片.png')
    expect(pastedFileName('image/jpeg')).toBe('粘贴的图片.jpg')
    expect(pastedFileName('image/webp')).toBe('粘贴的图片.webp')
    expect(pastedFileName('image/gif')).toBe('粘贴的图片.gif')
    expect(pastedFileName('image/png', 1)).toBe('粘贴的图片-2.png')
  })
})

describe('extractImage：拖入与粘贴文件', () => {
  it('接受拖入的 PNG 并读出真实字节', async () => {
    const result = await extractImage(dataWithFiles([makeFile('图 1.png', 'image/png')]), 'dropped')
    expect(result.kind).toBe('image')
    if (result.kind !== 'image') return
    expect(result.image.fileName).toBe('图 1.png')
    expect(result.image.origin).toBe('dropped')
    expect(Array.from(result.image.bytes)).toEqual(Array.from(PNG_BYTES))
  })

  it('接受粘贴的 JPG（从资源管理器复制文件）', async () => {
    const result = await extractImage(dataWithFiles([makeFile('photo.jpg', 'image/jpeg')]), 'pasted')
    expect(result.kind).toBe('image')
    if (result.kind !== 'image') return
    expect(result.image.fileName).toBe('photo.jpg')
    expect(result.image.origin).toBe('pasted')
  })

  it('文件名没有扩展名但 MIME 可接受时仍然接受', async () => {
    const result = await extractImage(dataWithFiles([makeFile('blob', 'image/png')]), 'dropped')
    expect(result.kind).toBe('image')
  })

  it('MIME 为空但扩展名可接受时仍然接受', async () => {
    const result = await extractImage(dataWithFiles([makeFile('figure.webp', '')]), 'dropped')
    expect(result.kind).toBe('image')
  })

  it('拒绝非图片文件并说明原因', async () => {
    const result = await extractImage(dataWithFiles([makeFile('note.md', 'text/markdown')]), 'dropped')
    expect(result.kind).toBe('rejected')
    if (result.kind !== 'rejected') return
    expect(result.reason).toContain('note.md')
    expect(result.reason).toContain('PNG')
  })

  it('拒绝 SVG（后端也不认该文件头）', async () => {
    const result = await extractImage(dataWithFiles([makeFile('vector.svg', 'image/svg+xml')]), 'dropped')
    expect(result.kind).toBe('rejected')
  })
})

describe('extractImage：剪贴板位图（截图）', () => {
  it('接受没有文件名的 PNG 位图并合成可读名字', async () => {
    const result = await extractImage(dataWithBitmap('image/png'), 'pasted')
    expect(result.kind).toBe('image')
    if (result.kind !== 'image') return
    expect(result.image.fileName).toBe('粘贴的图片.png')
    expect(result.image.bytes.length).toBe(PNG_BYTES.length)
  })

  it('接受没有文件名的 JPEG 位图并按 jpg 命名', async () => {
    const result = await extractImage(dataWithBitmap('image/jpeg'), 'pasted')
    expect(result.kind).toBe('image')
    if (result.kind !== 'image') return
    expect(result.image.fileName).toBe('粘贴的图片.jpg')
  })

  it('拒绝不支持的剪贴板位图格式', async () => {
    const result = await extractImage(dataWithBitmap('image/bmp'), 'pasted')
    expect(result.kind).toBe('rejected')
    if (result.kind !== 'rejected') return
    expect(result.reason).toContain('image/bmp')
  })

  it('位图读取失败时给出明确原因', async () => {
    const data = {
      files: [],
      items: [{ kind: 'file', type: 'image/png', getAsFile: () => null }],
      types: ['Files'],
    } as unknown as DataTransfer
    const result = await extractImage(data, 'pasted')
    expect(result.kind).toBe('rejected')
  })
})

describe('extractImage：与图片无关的内容应放行', () => {
  it('纯文本剪贴板返回 none（保持编辑器原生粘贴）', async () => {
    const data = { files: [], items: [], types: ['text/plain'] } as unknown as DataTransfer
    const result = await extractImage(data, 'pasted')
    expect(result.kind).toBe('none')
  })

  it('空 DataTransfer 返回 none', async () => {
    expect((await extractImage(null, 'pasted')).kind).toBe('none')
  })

  it('HTML 拖入（非文件）返回 none', async () => {
    const data = { files: [], items: [], types: ['text/html'] } as unknown as DataTransfer
    expect((await extractImage(data, 'dropped')).kind).toBe('none')
  })
})

describe('dragHasFiles', () => {
  it('含 Files 类型时返回 true', () => {
    expect(dragHasFiles({ types: ['Files'] } as unknown as DataTransfer)).toBe(true)
    expect(dragHasFiles({ types: ['Files', 'text/plain'] } as unknown as DataTransfer)).toBe(true)
  })

  it('不含 Files 类型或为空时返回 false', () => {
    expect(dragHasFiles({ types: ['text/plain'] } as unknown as DataTransfer)).toBe(false)
    expect(dragHasFiles(null)).toBe(false)
  })
})
