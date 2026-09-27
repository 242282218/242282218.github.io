/**
 * 后端错误映射测试。
 *
 * 重点：界面在没有连接到软件后端（例如直接用浏览器打开）时，
 * 必须给出**可操作**的中文说明，不能显示「发生未知错误」。
 */
import { describe, expect, it } from 'vitest'
import { BRIDGE_UNAVAILABLE_MESSAGE, isBridgeAvailable, toWriterError } from '@/services/backend'

describe('toWriterError：结构化错误直接透传', () => {
  it('保留后端返回的错误码、消息与定位信息', () => {
    const mapped = toWriterError({
      code: 'remote-changed',
      message: '远端已变化',
      detail: 'writing',
    })
    expect(mapped.code).toBe('remote-changed')
    expect(mapped.message).toBe('远端已变化')
    expect(mapped.detail).toBe('writing')
  })

  it('保留无 detail 的错误', () => {
    const mapped = toWriterError({ code: 'offline', message: '网络不可用' })
    expect(mapped.code).toBe('offline')
    expect(mapped.detail).toBeUndefined()
  })
})

describe('toWriterError：未连接到软件后端的说明', () => {
  it('识别 invoke 属性读取失败并给出可操作提示', () => {
    const error = new TypeError("Cannot read properties of undefined (reading 'invoke')")
    const mapped = toWriterError(error)
    expect(mapped.code).toBe('toolchain-missing')
    expect(mapped.message).toBe(BRIDGE_UNAVAILABLE_MESSAGE)
    expect(mapped.message).toContain('软件窗口')
  })

  it('识别字符串形式的同类错误', () => {
    const mapped = toWriterError('window.__TAURI_INTERNALS__ is undefined')
    expect(mapped.code).toBe('toolchain-missing')
    expect(mapped.message).toBe(BRIDGE_UNAVAILABLE_MESSAGE)
  })

  it('识别 transformCallback 缺失', () => {
    const mapped = toWriterError(
      new TypeError("Cannot read properties of undefined (reading 'transformCallback')"),
    )
    expect(mapped.code).toBe('toolchain-missing')
  })

  it('提示文案必须比「未知错误」更有操作性', () => {
    expect(BRIDGE_UNAVAILABLE_MESSAGE).not.toContain('未知')
    expect(BRIDGE_UNAVAILABLE_MESSAGE.length).toBeGreaterThan(10)
  })
})

describe('toWriterError：其它失败', () => {
  it('普通 Error 使用其 message', () => {
    const mapped = toWriterError(new Error('意外的解析失败'))
    expect(mapped.code).toBe('io-failed')
    expect(mapped.message).toBe('意外的解析失败')
  })

  it('既非 Error 也非对象时给出兜底文案', () => {
    const mapped = toWriterError(undefined)
    expect(mapped.code).toBe('io-failed')
    expect(mapped.message).toBe('发生未知错误')
  })

  it('无关的 undefined 属性错误不被误判为桥缺失', () => {
    const mapped = toWriterError(new TypeError("Cannot read properties of undefined (reading 'foo')"))
    expect(mapped.code).toBe('io-failed')
    expect(mapped.message).toContain('foo')
  })
})

describe('isBridgeAvailable', () => {
  it('在测试环境（无 Tauri 宿主）返回 false', () => {
    expect(isBridgeAvailable()).toBe(false)
  })
})
