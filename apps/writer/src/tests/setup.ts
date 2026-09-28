/**
 * vitest 全局前置：补齐 jsdom 缺失的浏览器 API。
 *
 * 只补「测试环境没有、真实浏览器有」的接口，不改被测代码的行为。
 */

/**
 * `Range.prototype.getClientRects` / `getBoundingClientRect`。
 *
 * CodeMirror 6 做光标定位（`coordsAtPos`）与选区绘制时会调用它们；jsdom 没有
 * 实现，于是每次渲染都抛 `TypeError: textRange(...).getClientRects is not a function`。
 * 这些异常不致命（组件仍能工作），但会把测试输出淹没，让真正的失败难以发现。
 * 返回空矩形是安全的下界：调用方会退化为「测量不到」，不会伪造出错误坐标。
 */
if (typeof Range !== 'undefined') {
  const emptyRect = {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
    top: 0,
    right: 0,
    bottom: 0,
    left: 0,
    toJSON: () => ({}),
  } as DOMRect

  if (typeof Range.prototype.getClientRects !== 'function') {
    Range.prototype.getClientRects = function getClientRects(): DOMRectList {
      const list = [] as unknown as DOMRectList & DOMRect[]
      Object.defineProperty(list, 'item', { value: (index: number) => list[index] ?? null })
      return list
    }
  }

  if (typeof Range.prototype.getBoundingClientRect !== 'function') {
    Range.prototype.getBoundingClientRect = function getBoundingClientRect(): DOMRect {
      return emptyRect
    }
  }
}

/**
 * `ResizeObserver`：reka-ui 的 `SplitterGroup` 用它跟踪面板尺寸。
 *
 * jsdom 没有实现它，挂载带分隔面板的组件会直接抛错。这里给出一个不回调的空
 * 实现：测试不依赖尺寸测量（那是真实窗口的验收项），只验证结构与接线。
 */
if (typeof globalThis.ResizeObserver !== 'function') {
  class ResizeObserverStub {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver
}
