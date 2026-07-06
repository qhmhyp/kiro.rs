import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { useState } from 'react'

export interface VerifyResult {
  id: number
  status: 'pending' | 'verifying' | 'success' | 'failed'
  latencyMs?: number
  statusCode?: number
  error?: string
}

const VERIFY_MODELS: { label: string; value: string }[] = [
  { label: 'Haiku 4.5', value: 'claude-haiku-4-5' },
  { label: 'Sonnet 4.6', value: 'claude-sonnet-4-6' },
  { label: 'Opus 4.7', value: 'claude-opus-4-7' },
  { label: 'Opus 4.8', value: 'claude-opus-4-8' },
  { label: 'Sonnet 5', value: 'claude-sonnet-5' },
]

interface BatchVerifyDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** true 表示已经开始过验证（用来切换"未开始 → 进行中/完成"的 UI） */
  hasStarted: boolean
  verifying: boolean
  progress: { current: number; total: number }
  results: Map<number, VerifyResult>
  /** 待验证凭据数量（用于"开始"前的预览） */
  selectedCount: number
  onStart: (model: string) => void
  onCancel: () => void
}

export function BatchVerifyDialog({
  open,
  onOpenChange,
  hasStarted,
  verifying,
  progress,
  results,
  selectedCount,
  onStart,
  onCancel,
}: BatchVerifyDialogProps) {
  const [model, setModel] = useState(VERIFY_MODELS[0].value)
  const resultsArray = Array.from(results.values())
  const successCount = resultsArray.filter(r => r.status === 'success').length
  const failedCount = resultsArray.filter(r => r.status === 'failed').length

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>批量验证凭据</DialogTitle>
        </DialogHeader>

        <div className="space-y-4 py-4">
          {/* 未开始：选模型 */}
          {!hasStarted && (
            <div className="space-y-3">
              <p className="text-sm">
                将对 <strong>{selectedCount}</strong> 个选中的凭据各发起一次最小 <code>messages</code> 请求，
                按 id 强制路由（disabled / cooldown 也会被测试）。
              </p>
              <div className="space-y-1.5">
                <label className="text-sm font-medium" htmlFor="batch-verify-model">
                  测试模型
                </label>
                <select
                  id="batch-verify-model"
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                  className="w-full h-9 rounded-md border bg-background px-2 text-sm"
                >
                  {VERIFY_MODELS.map((m) => (
                    <option key={m.value} value={m.value}>
                      {m.label}
                    </option>
                  ))}
                </select>
              </div>
            </div>
          )}

          {/* 进度 */}
          {verifying && (
            <div className="space-y-2">
              <div className="flex justify-between text-sm">
                <span>验证进度</span>
                <span>{progress.current} / {progress.total}</span>
              </div>
              <div className="w-full bg-secondary rounded-full h-2">
                <div
                  className="bg-primary h-2 rounded-full transition-all"
                  style={{
                    width: progress.total > 0
                      ? `${(progress.current / progress.total) * 100}%`
                      : '0%',
                  }}
                />
              </div>
            </div>
          )}

          {/* 统计 */}
          {results.size > 0 && (
            <div className="flex justify-between text-sm font-medium">
              <span>验证结果</span>
              <span>
                成功: {successCount} / 失败: {failedCount}
              </span>
            </div>
          )}

          {/* 结果列表 */}
          {results.size > 0 && (
            <div className="max-h-[400px] overflow-y-auto border rounded-md p-2 space-y-1">
              {resultsArray.map((result) => (
                <div
                  key={result.id}
                  className={`text-sm p-2 rounded ${
                    result.status === 'success'
                      ? 'bg-green-50 text-green-700 dark:bg-green-950 dark:text-green-300'
                      : result.status === 'failed'
                      ? 'bg-red-50 text-red-700 dark:bg-red-950 dark:text-red-300'
                      : result.status === 'verifying'
                      ? 'bg-blue-50 text-blue-700 dark:bg-blue-950 dark:text-blue-300'
                      : 'bg-gray-50 text-gray-700 dark:bg-gray-950 dark:text-gray-300'
                  }`}
                >
                  <div className="flex items-start justify-between gap-2">
                    <div className="flex items-center gap-2">
                      <span className="font-medium">凭据 #{result.id}</span>
                      {result.status === 'success' && result.latencyMs !== undefined && (
                        <Badge variant="secondary" className="text-xs">
                          {result.latencyMs}ms
                        </Badge>
                      )}
                      {result.status === 'failed' && result.statusCode !== undefined && (
                        <Badge variant="outline" className="text-xs">
                          HTTP {result.statusCode}
                        </Badge>
                      )}
                    </div>
                    <span>
                      {result.status === 'success' && '✓'}
                      {result.status === 'failed' && '✗'}
                      {result.status === 'verifying' && '⏳'}
                      {result.status === 'pending' && '⋯'}
                    </span>
                  </div>
                  {result.error && (
                    <div className="text-xs mt-1 opacity-90 break-words">
                      {result.error}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}

          {verifying && (
            <p className="text-xs text-muted-foreground">
              💡 每次请求间隔 2 秒避免风控。可关闭此窗口，验证会在后台继续。
            </p>
          )}
        </div>

        <div className="flex justify-end gap-2">
          {!hasStarted && (
            <>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
              >
                取消
              </Button>
              <Button
                type="button"
                onClick={() => onStart(model)}
                disabled={selectedCount === 0}
              >
                开始验证
              </Button>
            </>
          )}
          {hasStarted && verifying && (
            <>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
              >
                后台运行
              </Button>
              <Button
                type="button"
                variant="destructive"
                onClick={onCancel}
              >
                取消验证
              </Button>
            </>
          )}
          {hasStarted && !verifying && (
            <Button
              type="button"
              onClick={() => onOpenChange(false)}
            >
              关闭
            </Button>
          )}
        </div>
      </DialogContent>
    </Dialog>
  )
}
