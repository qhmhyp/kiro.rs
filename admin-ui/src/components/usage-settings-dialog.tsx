import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { getUsageCacheSettings, updateUsageCacheSettings } from '@/api/settings'
import { extractErrorMessage } from '@/lib/utils'

interface UsageSettingsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function UsageSettingsDialog({ open, onOpenChange }: UsageSettingsDialogProps) {
  const [loading, setLoading] = useState(false)
  const [saving, setSaving] = useState(false)
  const [enabled, setEnabled] = useState(true)
  const [idleSecs, setIdleSecs] = useState('300')
  const [readRatio, setReadRatio] = useState('1')

  // 打开时拉取当前生效值
  useEffect(() => {
    if (!open) return
    setLoading(true)
    getUsageCacheSettings()
      .then((s) => {
        setEnabled(s.enabled)
        setIdleSecs(String(s.idleSecs))
        setReadRatio(String(s.readRatio))
      })
      .catch((err) => {
        toast.error(`加载设置失败: ${extractErrorMessage(err)}`)
        onOpenChange(false)
      })
      .finally(() => setLoading(false))
  }, [open]) // eslint-disable-line react-hooks/exhaustive-deps

  const handleSave = async () => {
    const idle = Number(idleSecs)
    const ratio = Number(readRatio)
    if (!Number.isFinite(idle) || idle < 0 || !Number.isInteger(idle)) {
      toast.error('过期时间必须是非负整数（秒）')
      return
    }
    if (!Number.isFinite(ratio) || ratio < 0 || ratio > 1) {
      toast.error('折扣比例必须在 0 ~ 1 之间')
      return
    }

    setSaving(true)
    try {
      const result = await updateUsageCacheSettings({
        enabled,
        idleSecs: idle,
        readRatio: ratio,
      })
      if (result.persistWarning) {
        toast.warning(result.persistWarning)
      } else {
        toast.success('usage 上报设置已保存并即时生效')
      }
      onOpenChange(false)
    } catch (err) {
      toast.error(`保存失败: ${extractErrorMessage(err)}`)
    } finally {
      setSaving(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>usage 上报设置</DialogTitle>
        </DialogHeader>

        {loading ? (
          <div className="flex items-center justify-center py-8">
            <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
          </div>
        ) : (
          <div className="space-y-5">
            <p className="text-sm text-muted-foreground">
              控制返回给客户端的 usage 字段如何拆分 prompt cache 命中（cache_read 按
              0.1×、cache_creation 按 1.25×、input 按 1× 计费）。不影响真实上游调用，
              修改后立即生效并写回 config.json。
            </p>

            <div className="flex items-center justify-between">
              <div className="space-y-0.5">
                <div className="text-sm font-medium">模拟 prompt cache 命中</div>
                <div className="text-xs text-muted-foreground">
                  关闭后全部输入按 input_tokens 全价上报（计费最高）
                </div>
              </div>
              <Switch checked={enabled} onCheckedChange={setEnabled} />
            </div>

            <div className="space-y-1.5">
              <div className="text-sm font-medium">会话空闲过期时间（秒）</div>
              <Input
                type="number"
                min={0}
                step={1}
                value={idleSecs}
                onChange={(e) => setIdleSecs(e.target.value)}
                disabled={!enabled}
              />
              <div className="text-xs text-muted-foreground">
                空闲超时后下一轮按"重建缓存"（1.25×）重计一轮；0 = 永不过期。
                默认 300（对齐 Anthropic）。调小可提高计费。
              </div>
            </div>

            <div className="space-y-1.5">
              <div className="text-sm font-medium">cache_read 折扣比例（0 ~ 1）</div>
              <Input
                type="number"
                min={0}
                max={1}
                step={0.05}
                value={readRatio}
                onChange={(e) => setReadRatio(e.target.value)}
                disabled={!enabled}
              />
              <div className="text-xs text-muted-foreground">
                命中部分按该比例享受 0.1× 折扣价，其余按全价计入 input_tokens。
                1 = 全折扣（默认），0.5 = 折扣减半，0 = 无折扣。
              </div>
            </div>

            <div className="flex justify-end gap-2 pt-2">
              <Button variant="outline" onClick={() => onOpenChange(false)} disabled={saving}>
                取消
              </Button>
              <Button onClick={handleSave} disabled={saving}>
                {saving ? '保存中...' : '保存'}
              </Button>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
