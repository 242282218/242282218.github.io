/**
 * 剪贴板粘贴与拖入图片的解析。
 *
 * 这里只负责「从事件里取出图片字节与文件名」，不接触后端；
 * 归档与校验统一由后端 `import_article_image_bytes` 完成（与文件选择插入
 * 共用同一套文件头与大小校验）。
 *
 * 支持来源：
 * - 剪贴板粘贴（`paste` 事件）：截图工具常以 `image/png` 位图放进剪贴板；
 *   从资源管理器复制文件时也可能以 `Files` 出现；
 * - 拖入（`drop` 事件）：从资源管理器拖入的图片文件。
 *
 * 明确不支持、也不假装支持：把**磁盘路径**直接交给后端插入。浏览器出于安全
 * 不会在拖入时给出真实路径，因此拖入与粘贴都走「读取字节 → 后端归档」这条
 * 与文件选择插入同等校验强度的路径。
 */

/** 允许插入的图片 MIME 类型（与后端接受的文件头一致）。 */
export const ACCEPTED_IMAGE_MIME = new Set([
  'image/png',
  'image/jpeg',
  'image/jpg',
  'image/webp',
  'image/gif',
])

/** 允许的扩展名（浏览器不提供 MIME 时按文件名判断）。 */
const ACCEPTED_EXTENSIONS = ['.png', '.jpg', '.jpeg', '.webp', '.gif']

/** 一张待插入的图片。 */
export type PickedImage = {
  fileName: string
  bytes: Uint8Array
  origin: 'pasted' | 'dropped'
}

/** 解析结果：一张图片、明确的拒绝原因，或与图片无关（应放行）。 */
export type ImagePickResult =
  | { kind: 'image'; image: PickedImage }
  | { kind: 'rejected'; reason: string }
  | { kind: 'none' }

/** 按文件名扩展名判断是否为可插入图片。 */
export function isAcceptedFileName(fileName: string): boolean {
  const lower = fileName.toLowerCase()
  return ACCEPTED_EXTENSIONS.some((extension) => lower.endsWith(extension))
}

/** 按 MIME 类型判断是否为可插入图片。 */
export function isAcceptedMime(mime: string): boolean {
  return ACCEPTED_IMAGE_MIME.has(mime.toLowerCase())
}

/** 为剪贴板中的位图（没有文件名）生成一个可读名字。 */
export function pastedFileName(mime: string, index = 0): string {
  const normalized = mime.toLowerCase()
  const extension =
    normalized === 'image/jpeg' || normalized === 'image/jpg'
      ? 'jpg'
      : normalized === 'image/webp'
        ? 'webp'
        : normalized === 'image/gif'
          ? 'gif'
          : 'png'
  return index > 0 ? `粘贴的图片-${index + 1}.${extension}` : `粘贴的图片.${extension}`
}

/** 读取图片文件的字节。 */
export async function readImageBytes(file: File): Promise<Uint8Array> {
  const buffer = await file.arrayBuffer()
  return new Uint8Array(buffer)
}

/** 判断候选文件是否可接受（文件名或 MIME 任一命中即可）。 */
function isAcceptableFile(file: File): boolean {
  return isAcceptedFileName(file.name) || (file.type !== '' && isAcceptedMime(file.type))
}

/** 组装结果。 */
async function toPicked(file: File, name: string, origin: 'pasted' | 'dropped'): Promise<ImagePickResult> {
  return {
    kind: 'image',
    image: { fileName: name, bytes: await readImageBytes(file), origin },
  }
}

/**
 * 从 `DataTransfer` 取出第一张可插入的图片。
 *
 * 判定顺序（尽量不漏掉截图，也不误判其它内容）：
 * 1. `files`：从资源管理器复制或拖入的文件，按文件名与 MIME 校验；
 * 2. `items` 中 `kind === 'file'` 且 `image/*`：剪贴板位图，此时合成可读文件名。
 *
 * 与图片无关的内容返回 `none`，由调用方决定是否放行默认行为。
 */
export async function extractImage(
  data: DataTransfer | null,
  origin: 'pasted' | 'dropped',
): Promise<ImagePickResult> {
  if (!data) return { kind: 'none' }

  const files = Array.from(data.files ?? [])
  const first = files[0]
  if (first) {
    if (!isAcceptableFile(first)) {
      return {
        kind: 'rejected',
        reason: `只支持 PNG、JPEG、WebP、GIF 图片；「${first.name || first.type || '未知文件'}」不是支持的格式`,
      }
    }
    return toPicked(first, first.name || pastedFileName(first.type), origin)
  }

  const items = Array.from(data.items ?? [])
  for (let index = 0; index < items.length; index += 1) {
    const item = items[index]
    if (!item || item.kind !== 'file') continue
    const mime = item.type.toLowerCase()
    if (!mime.startsWith('image/')) continue
    if (!isAcceptedMime(mime)) {
      return {
        kind: 'rejected',
        reason: `剪贴板中的图片格式（${mime}）不受支持；请使用 PNG、JPEG、WebP 或 GIF`,
      }
    }
    const file = item.getAsFile()
    if (!file) {
      return { kind: 'rejected', reason: '无法读取剪贴板中的图片内容' }
    }
    return toPicked(file, file.name || pastedFileName(mime, index), origin)
  }

  return { kind: 'none' }
}

/** 拖入内容是否包含文件（用于决定是否拦截默认行为）。 */
export function dragHasFiles(data: DataTransfer | null): boolean {
  if (!data) return false
  return Array.from(data.types ?? []).includes('Files')
}
