import { useState } from 'react'
import { toast } from 'sonner'
import {
  Trash2,
  Loader2,
  Wallet,
  KeyRound,
  PlayCircle,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type {
  CredentialStatusItem,
  BalanceResponse,
  VerifyMessageResponse,
} from '@/types/api'
import {
  useSetDisabled,
  useSetPriority,
  useDeleteCredential,
  useForceRefreshToken,
} from '@/hooks/use-credentials'
import { verifyCredentialMessage } from '@/api/credentials'
import { cn } from '@/lib/utils'

interface CredentialRowProps {
  credential: CredentialStatusItem
  onViewBalance: (id: number) => void
  selected: boolean
  onToggleSelect: () => void
  balance: BalanceResponse | null
  loadingBalance: boolean
}

const VERIFY_MODELS: { label: string; value: string }[] = [
  { label: 'Haiku 4.5', value: 'claude-haiku-4-5' },
  { label: 'Sonnet 4.6', value: 'claude-sonnet-4-6' },
  { label: 'Opus 4.7', value: 'claude-opus-4-7' },
]

function formatLastUsed(lastUsedAt: string | null): string {
  if (!lastUsedAt) return '从未使用'
  const date = new Date(lastUsedAt)
  const now = new Date()
  const diff = now.getTime() - date.getTime()
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds}s 前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m 前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h 前`
  const days = Math.floor(hours / 24)
  return `${days}d 前`
}

function authMethodLabel(method?: string): string {
  if (method === 'api_key') return 'API Key'
  if (method === 'idc') return 'IdC'
  if (method === 'social') return 'Social'
  return method ?? '-'
}

export function CredentialRow({
  credential,
  onViewBalance,
  selected,
  onToggleSelect,
  balance,
  loadingBalance,
}: CredentialRowProps) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
  const [verifyModel, setVerifyModel] = useState(VERIFY_MODELS[0].value)
  const [verifying, setVerifying] = useState(false)
  const [verifyResult, setVerifyResult] = useState<VerifyMessageResponse | null>(null)

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const deleteCredential = useDeleteCredential()
  const forceRefresh = useForceRefreshToken()

  const handleToggleDisabled = () => {
    setDisabled.mutate(
      { id: credential.id, disabled: !credential.disabled },
      {
        onSuccess: (res) => toast.success(res.message),
        onError: (err) => toast.error('操作失败: ' + (err as Error).message),
      }
    )
  }

  const handlePrioritySave = () => {
    const newPriority = parseInt(priorityValue, 10)
    if (isNaN(newPriority) || newPriority < 0) {
      toast.error('优先级必须是非负整数')
      return
    }
    setPriority.mutate(
      { id: credential.id, priority: newPriority },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingPriority(false)
        },
        onError: (err) => toast.error('操作失败: ' + (err as Error).message),
      }
    )
  }

  const handleVerify = async () => {
    setVerifying(true)
    setVerifyResult(null)
    try {
      const res = await verifyCredentialMessage(credential.id, verifyModel)
      setVerifyResult(res)
      if (res.ok) {
        toast.success(`#${credential.id} 验证通过 (${res.latencyMs}ms)`)
      } else {
        toast.error(`#${credential.id} 验证失败: ${res.status ?? '-'} ${res.error ?? ''}`)
      }
    } catch (err) {
      const msg = (err as Error).message
      setVerifyResult({ ok: false, latencyMs: 0, error: msg })
      toast.error('验证请求失败: ' + msg)
    } finally {
      setVerifying(false)
    }
  }

  const handleDelete = () => {
    if (!credential.disabled) {
      toast.error('请先禁用凭据再删除')
      setShowDeleteDialog(false)
      return
    }
    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
        setShowDeleteDialog(false)
      },
      onError: (err) => toast.error('删除失败: ' + (err as Error).message),
    })
  }

  const usage = loadingBalance ? (
    <Loader2 className="inline w-3 h-3 animate-spin" />
  ) : balance ? (
    <span
      className="text-xs whitespace-nowrap"
      title={`已用 ${balance.usagePercentage.toFixed(1)}%（可超额计费）`}
    >
      {balance.currentUsage.toFixed(1)}/{balance.usageLimit.toFixed(0)}
    </span>
  ) : (
    <span className="text-xs text-muted-foreground">-</span>
  )

  const subscription = loadingBalance
    ? '...'
    : balance?.subscriptionTitle ?? '-'

  return (
    <>
      <tr
        className={cn(
          'border-b transition-colors hover:bg-muted/40',
          credential.isCurrent && 'bg-primary/5'
        )}
      >
        {/* 选择框 */}
        <td className="px-2 py-2">
          <Checkbox checked={selected} onCheckedChange={onToggleSelect} />
        </td>

        {/* ID / 邮箱 */}
        <td className="px-2 py-2">
          <div className="flex flex-col">
            <div className="flex items-center gap-1.5">
              <span className="font-medium text-sm">
                {credential.email || `#${credential.id}`}
              </span>
              {credential.isCurrent && (
                <Badge variant="success" className="h-4 px-1 text-[10px]">当前</Badge>
              )}
            </div>
            <div className="flex flex-wrap items-center gap-1 mt-0.5">
              <span className="text-[10px] text-muted-foreground">#{credential.id}</span>
              {credential.authMethod && (
                <Badge variant="secondary" className="h-4 px-1 text-[10px]">
                  {authMethodLabel(credential.authMethod)}
                </Badge>
              )}
              {credential.endpoint && (
                <Badge variant="outline" className="h-4 px-1 text-[10px]">
                  {credential.endpoint}
                </Badge>
              )}
            </div>
          </div>
        </td>

        {/* 订阅类型 */}
        <td className="px-2 py-2 text-sm whitespace-nowrap">{subscription}</td>

        {/* 优先级 */}
        <td className="px-2 py-2">
          {editingPriority ? (
            <div className="flex items-center gap-0.5">
              <Input
                type="number"
                value={priorityValue}
                onChange={(e) => setPriorityValue(e.target.value)}
                className="w-14 h-6 text-xs px-1"
                min="0"
              />
              <Button
                size="sm"
                variant="ghost"
                className="h-6 w-6 p-0 text-xs"
                onClick={handlePrioritySave}
                disabled={setPriority.isPending}
              >
                ✓
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="h-6 w-6 p-0 text-xs"
                onClick={() => {
                  setEditingPriority(false)
                  setPriorityValue(String(credential.priority))
                }}
              >
                ✕
              </Button>
            </div>
          ) : (
            <span
              className="font-medium text-sm cursor-pointer hover:underline px-1"
              onClick={() => setEditingPriority(true)}
              title="点击编辑优先级"
            >
              {credential.priority}
            </span>
          )}
        </td>

        {/* 状态 */}
        <td className="px-2 py-2">
          <div className="flex items-center gap-1">
            <Switch
              checked={!credential.disabled}
              onCheckedChange={handleToggleDisabled}
              disabled={setDisabled.isPending}
            />
            {credential.disabled && credential.disabledReason && (
              <Badge variant="outline" className="h-4 px-1 text-[10px]">
                {credential.disabledReason}
              </Badge>
            )}
          </div>
        </td>

        {/* 失败 / 用量 */}
        <td className="px-2 py-2 text-sm whitespace-nowrap">
          <div className="flex flex-col gap-0.5">
            <div className="text-xs">
              <span
                className={
                  credential.failureCount > 0 ? 'text-red-500 font-medium' : 'text-muted-foreground'
                }
                title="API 失败次数"
              >
                f:{credential.failureCount}
              </span>{' '}
              <span
                className={
                  credential.refreshFailureCount > 0
                    ? 'text-red-500 font-medium'
                    : 'text-muted-foreground'
                }
                title="Token 刷新失败次数"
              >
                r:{credential.refreshFailureCount}
              </span>{' '}
              <span className="text-muted-foreground" title="成功次数">
                s:{credential.successCount}
              </span>
            </div>
            <div>{usage}</div>
          </div>
        </td>

        {/* 最近使用 */}
        <td className="px-2 py-2 text-xs text-muted-foreground whitespace-nowrap">
          {formatLastUsed(credential.lastUsedAt)}
        </td>

        {/* 验证 */}
        <td className="px-2 py-2">
          <div className="flex items-center gap-1">
            <select
              value={verifyModel}
              onChange={(e) => setVerifyModel(e.target.value)}
              disabled={verifying}
              className="h-7 rounded border bg-background px-1.5 text-xs"
            >
              {VERIFY_MODELS.map((m) => (
                <option key={m.value} value={m.value}>
                  {m.label}
                </option>
              ))}
            </select>
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2"
              onClick={handleVerify}
              disabled={verifying}
              title="用所选模型发一次 messages 请求测试该凭据"
            >
              {verifying ? (
                <Loader2 className="h-3 w-3 animate-spin" />
              ) : (
                <PlayCircle className="h-3 w-3" />
              )}
              <span className="ml-1 text-xs">验证</span>
            </Button>
            {verifyResult && !verifying && (
              <span
                className={cn(
                  'text-xs font-medium',
                  verifyResult.ok ? 'text-green-600' : 'text-red-500'
                )}
                title={verifyResult.error ?? ''}
              >
                {verifyResult.ok
                  ? `✓ ${verifyResult.latencyMs}ms`
                  : `✗ ${verifyResult.status ?? '-'}`}
              </span>
            )}
          </div>
        </td>

        {/* 其它操作 */}
        <td className="px-2 py-2">
          <div className="flex items-center gap-1">
            <Button
              size="sm"
              variant="ghost"
              className="h-7 w-7 p-0"
              onClick={() =>
                forceRefresh.mutate(credential.id, {
                  onSuccess: (res) => toast.success(res.message),
                  onError: (err) => toast.error('刷新失败: ' + (err as Error).message),
                })
              }
              disabled={
                forceRefresh.isPending ||
                credential.disabled ||
                credential.authMethod === 'api_key'
              }
              title="强制刷新 Token"
            >
              <KeyRound
                className={cn('h-3.5 w-3.5', forceRefresh.isPending && 'animate-pulse')}
              />
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="h-7 w-7 p-0"
              onClick={() => onViewBalance(credential.id)}
              title="查看余额详情"
            >
              <Wallet className="h-3.5 w-3.5" />
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="h-7 w-7 p-0 text-red-500 hover:text-red-600 hover:bg-red-50"
              onClick={() => setShowDeleteDialog(true)}
              disabled={!credential.disabled}
              title={!credential.disabled ? '需要先禁用凭据才能删除' : '删除凭据'}
            >
              <Trash2 className="h-3.5 w-3.5" />
            </Button>
          </div>
        </td>
      </tr>

      {/* 删除确认对话框 */}
      <Dialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除凭据</DialogTitle>
            <DialogDescription>
              您确定要删除凭据 #{credential.id} 吗？此操作无法撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowDeleteDialog(false)}
              disabled={deleteCredential.isPending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteCredential.isPending || !credential.disabled}
            >
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
