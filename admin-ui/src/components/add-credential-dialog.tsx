import { useState, useEffect } from 'react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useAddCredential, useUpdateCredential } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { CredentialStatusItem, UpdateCredentialRequest } from '@/types/api'

interface AddCredentialDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** 'add' = 新建；'edit' = 编辑 editTarget 指向的凭据 */
  mode?: 'add' | 'edit'
  /** edit 模式必须传，用于预填已知字段 */
  editTarget?: CredentialStatusItem | null
}

type AuthMethod = 'social' | 'idc' | 'api_key'

const initialFormState = () => ({
  name: '',
  refreshToken: '',
  kiroApiKey: '',
  authMethod: 'social' as AuthMethod,
  authRegion: '',
  apiRegion: '',
  clientId: '',
  clientSecret: '',
  priority: '0',
  machineId: '',
  proxyUrl: '',
  proxyUsername: '',
  proxyPassword: '',
  endpoint: '',
})

export function AddCredentialDialog({
  open,
  onOpenChange,
  mode = 'add',
  editTarget = null,
}: AddCredentialDialogProps) {
  const [form, setForm] = useState(initialFormState)
  const isEdit = mode === 'edit' && editTarget !== null

  const addMutation = useAddCredential()
  const updateMutation = useUpdateCredential()
  const isPending = addMutation.isPending || updateMutation.isPending

  // 打开时预填 / 关闭时重置
  useEffect(() => {
    if (!open) return
    if (isEdit && editTarget) {
      setForm({
        ...initialFormState(),
        name: editTarget.name ?? '',
        authMethod: (editTarget.authMethod as AuthMethod) || 'social',
        priority: String(editTarget.priority),
        endpoint: editTarget.endpoint ?? '',
        proxyUrl: editTarget.proxyUrl ?? '',
      })
    } else {
      setForm(initialFormState())
    }
  }, [open, isEdit, editTarget])

  const set = <K extends keyof ReturnType<typeof initialFormState>>(
    key: K,
    value: ReturnType<typeof initialFormState>[K]
  ) => setForm(prev => ({ ...prev, [key]: value }))

  const isApiKey = form.authMethod === 'api_key'

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()

    if (isEdit && editTarget) {
      // 构造 PATCH payload：留空字段不发送
      const trim = (s: string) => s.trim()
      const opt = (s: string) => (trim(s) ? trim(s) : undefined)
      const payload: UpdateCredentialRequest = {
        name: opt(form.name),
        refreshToken: opt(form.refreshToken),
        kiroApiKey: opt(form.kiroApiKey),
        clientId: opt(form.clientId),
        clientSecret: opt(form.clientSecret),
        authRegion: opt(form.authRegion),
        apiRegion: opt(form.apiRegion),
        machineId: opt(form.machineId),
        proxyUrl: opt(form.proxyUrl),
        proxyUsername: opt(form.proxyUsername),
        proxyPassword: opt(form.proxyPassword),
        endpoint: opt(form.endpoint),
        priority: Number.isFinite(parseInt(form.priority))
          ? parseInt(form.priority)
          : undefined,
      }
      // 过滤掉 undefined 字段，避免序列化时出现 explicit null
      Object.keys(payload).forEach(k => {
        if ((payload as Record<string, unknown>)[k] === undefined) {
          delete (payload as Record<string, unknown>)[k]
        }
      })

      if (Object.keys(payload).length === 0) {
        toast.info('没有需要修改的字段')
        return
      }

      updateMutation.mutate(
        { id: editTarget.id, payload },
        {
          onSuccess: (data) => {
            toast.success(data.message)
            onOpenChange(false)
          },
          onError: (error: unknown) => {
            toast.error(`更新失败: ${extractErrorMessage(error)}`)
          },
        }
      )
      return
    }

    // ===== 新建模式 =====
    if (isApiKey) {
      if (!form.kiroApiKey.trim()) {
        toast.error('请输入 Kiro API Key')
        return
      }
    } else {
      if (!form.refreshToken.trim()) {
        toast.error('请输入 Refresh Token')
        return
      }
      if (form.authMethod === 'idc' && (!form.clientId.trim() || !form.clientSecret.trim())) {
        toast.error('IdC/Builder-ID/IAM 认证需要填写 Client ID 和 Client Secret')
        return
      }
    }

    addMutation.mutate(
      {
        authMethod: form.authMethod,
        refreshToken: isApiKey ? undefined : form.refreshToken.trim(),
        kiroApiKey: isApiKey ? form.kiroApiKey.trim() : undefined,
        authRegion: form.authRegion.trim() || undefined,
        apiRegion: form.apiRegion.trim() || undefined,
        clientId: isApiKey ? undefined : form.clientId.trim() || undefined,
        clientSecret: isApiKey ? undefined : form.clientSecret.trim() || undefined,
        priority: parseInt(form.priority) || 0,
        machineId: form.machineId.trim() || undefined,
        proxyUrl: form.proxyUrl.trim() || undefined,
        proxyUsername: form.proxyUsername.trim() || undefined,
        proxyPassword: form.proxyPassword.trim() || undefined,
        endpoint: form.endpoint.trim() || undefined,
      },
      {
        onSuccess: (data) => {
          toast.success(data.message)
          onOpenChange(false)
        },
        onError: (error: unknown) => {
          toast.error(`添加失败: ${extractErrorMessage(error)}`)
        },
      }
    )
  }

  const placeholder = (def: string) => (isEdit ? '留空保持原值' : def)

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg max-h-[85vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>
            {isEdit ? `编辑凭据 #${editTarget?.id}` : '添加凭据'}
          </DialogTitle>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="flex flex-col min-h-0 flex-1">
          <div className="space-y-4 py-4 overflow-y-auto flex-1 pr-1">
            {isEdit && (
              <p className="text-xs text-muted-foreground -mt-2">
                所有字段留空表示不修改；填入新值将覆盖。authMethod 不可改（请删除后重新添加）。
              </p>
            )}

            {/* 自定义名称 */}
            {isEdit && (
              <div className="space-y-2">
                <label htmlFor="name" className="text-sm font-medium">
                  名称
                </label>
                <Input
                  id="name"
                  placeholder="留空保持原值；用于在列表中替代 email 显示"
                  value={form.name}
                  onChange={(e) => set('name', e.target.value)}
                  disabled={isPending}
                />
                <p className="text-xs text-muted-foreground">
                  自定义凭据显示名，优先级高于 email
                </p>
              </div>
            )}

            {/* 认证方式 */}
            <div className="space-y-2">
              <label htmlFor="authMethod" className="text-sm font-medium">
                认证方式
              </label>
              <select
                id="authMethod"
                value={form.authMethod}
                onChange={(e) => set('authMethod', e.target.value as AuthMethod)}
                disabled={isPending || isEdit}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value="social">Social</option>
                <option value="idc">IdC/Builder-ID/IAM</option>
                <option value="api_key">API Key</option>
              </select>
            </div>

            {/* Kiro API Key (API Key 模式) */}
            {isApiKey && (
              <div className="space-y-2">
                <label htmlFor="kiroApiKey" className="text-sm font-medium">
                  Kiro API Key {!isEdit && <span className="text-red-500">*</span>}
                </label>
                <Input
                  id="kiroApiKey"
                  type="password"
                  placeholder={placeholder('格式: ksk_xxxxxxxx')}
                  value={form.kiroApiKey}
                  onChange={(e) => set('kiroApiKey', e.target.value)}
                  disabled={isPending}
                />
              </div>
            )}

            {/* Refresh Token (OAuth 模式) */}
            {!isApiKey && (
              <div className="space-y-2">
                <label htmlFor="refreshToken" className="text-sm font-medium">
                  Refresh Token {!isEdit && <span className="text-red-500">*</span>}
                </label>
                <Input
                  id="refreshToken"
                  type="password"
                  placeholder={placeholder('请输入 Refresh Token')}
                  value={form.refreshToken}
                  onChange={(e) => set('refreshToken', e.target.value)}
                  disabled={isPending}
                />
              </div>
            )}

            {/* Region 配置 */}
            <div className="space-y-2">
              <label className="text-sm font-medium">Region 配置</label>
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <Input
                    id="authRegion"
                    placeholder={placeholder('Auth Region')}
                    value={form.authRegion}
                    onChange={(e) => set('authRegion', e.target.value)}
                    disabled={isPending}
                  />
                </div>
                <div>
                  <Input
                    id="apiRegion"
                    placeholder={placeholder('API Region')}
                    value={form.apiRegion}
                    onChange={(e) => set('apiRegion', e.target.value)}
                    disabled={isPending}
                  />
                </div>
              </div>
              <p className="text-xs text-muted-foreground">
                {isEdit
                  ? '留空不修改；填入新值会覆盖。'
                  : '均可留空使用全局配置。Auth Region 用于 Token 刷新，API Region 用于 API 请求'}
              </p>
            </div>

            {/* IdC/Builder-ID/IAM 额外字段 */}
            {form.authMethod === 'idc' && (
              <>
                <div className="space-y-2">
                  <label htmlFor="clientId" className="text-sm font-medium">
                    Client ID {!isEdit && <span className="text-red-500">*</span>}
                  </label>
                  <Input
                    id="clientId"
                    placeholder={placeholder('请输入 Client ID')}
                    value={form.clientId}
                    onChange={(e) => set('clientId', e.target.value)}
                    disabled={isPending}
                  />
                </div>
                <div className="space-y-2">
                  <label htmlFor="clientSecret" className="text-sm font-medium">
                    Client Secret {!isEdit && <span className="text-red-500">*</span>}
                  </label>
                  <Input
                    id="clientSecret"
                    type="password"
                    placeholder={placeholder('请输入 Client Secret')}
                    value={form.clientSecret}
                    onChange={(e) => set('clientSecret', e.target.value)}
                    disabled={isPending}
                  />
                </div>
              </>
            )}

            {/* 优先级 */}
            <div className="space-y-2">
              <label htmlFor="priority" className="text-sm font-medium">
                优先级
              </label>
              <Input
                id="priority"
                type="number"
                min="0"
                placeholder="数字越小优先级越高"
                value={form.priority}
                onChange={(e) => set('priority', e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                数字越小优先级越高
              </p>
            </div>

            {/* Machine ID */}
            <div className="space-y-2">
              <label htmlFor="machineId" className="text-sm font-medium">
                Machine ID
              </label>
              <Input
                id="machineId"
                placeholder={placeholder('留空使用配置中字段, 否则由刷新Token自动派生')}
                value={form.machineId}
                onChange={(e) => set('machineId', e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                可选，64 位十六进制字符串
              </p>
            </div>

            {/* 端点 */}
            <div className="space-y-2">
              <label htmlFor="endpoint" className="text-sm font-medium">
                端点
              </label>
              <Input
                id="endpoint"
                placeholder={placeholder('留空使用默认端点（如 ide / cli）')}
                value={form.endpoint}
                onChange={(e) => set('endpoint', e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                决定该凭据走哪套 Kiro API
              </p>
            </div>

            {/* 代理配置 */}
            <div className="space-y-2">
              <label className="text-sm font-medium">代理配置</label>
              <Input
                id="proxyUrl"
                placeholder={placeholder('代理 URL（"direct" 不使用代理）')}
                value={form.proxyUrl}
                onChange={(e) => set('proxyUrl', e.target.value)}
                disabled={isPending}
              />
              <div className="grid grid-cols-2 gap-2">
                <Input
                  id="proxyUsername"
                  placeholder={placeholder('代理用户名')}
                  value={form.proxyUsername}
                  onChange={(e) => set('proxyUsername', e.target.value)}
                  disabled={isPending}
                />
                <Input
                  id="proxyPassword"
                  type="password"
                  placeholder={placeholder('代理密码')}
                  value={form.proxyPassword}
                  onChange={(e) => set('proxyPassword', e.target.value)}
                  disabled={isPending}
                />
              </div>
              <p className="text-xs text-muted-foreground">
                输入 "direct" 可显式不使用代理
              </p>
            </div>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={isPending}
            >
              取消
            </Button>
            <Button type="submit" disabled={isPending}>
              {isPending
                ? isEdit
                  ? '保存中...'
                  : '添加中...'
                : isEdit
                  ? '保存'
                  : '添加'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
