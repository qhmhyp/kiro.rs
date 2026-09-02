import { useState, useEffect, useRef } from 'react'
import { RefreshCw, LogOut, Moon, Sun, Server, Plus, Upload, FileUp, Trash2, RotateCcw, PlayCircle, Wallet, SlidersHorizontal } from 'lucide-react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { storage } from '@/lib/storage'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import { CredentialRow } from '@/components/credential-row'
import { BalanceDialog } from '@/components/balance-dialog'
import { AddCredentialDialog } from '@/components/add-credential-dialog'
import { BatchImportDialog } from '@/components/batch-import-dialog'
import { KamImportDialog } from '@/components/kam-import-dialog'
import { BatchVerifyDialog, type VerifyResult } from '@/components/batch-verify-dialog'
import { UsageSettingsDialog } from '@/components/usage-settings-dialog'
import { useCredentials, useDeleteCredential, useResetFailure } from '@/hooks/use-credentials'
import { getCredentialBalance, forceRefreshToken, verifyCredentialMessage } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { BalanceResponse, CredentialStatusItem } from '@/types/api'

interface DashboardProps {
  onLogout: () => void
}

export function Dashboard({ onLogout }: DashboardProps) {
  const [selectedCredentialId, setSelectedCredentialId] = useState<number | null>(null)
  const [balanceDialogOpen, setBalanceDialogOpen] = useState(false)
  const [addDialogOpen, setAddDialogOpen] = useState(false)
  const [editTarget, setEditTarget] = useState<CredentialStatusItem | null>(null)
  const [batchImportDialogOpen, setBatchImportDialogOpen] = useState(false)
  const [kamImportDialogOpen, setKamImportDialogOpen] = useState(false)
  const [usageSettingsOpen, setUsageSettingsOpen] = useState(false)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [verifyDialogOpen, setVerifyDialogOpen] = useState(false)
  const [verifying, setVerifying] = useState(false)
  const [hasStartedVerify, setHasStartedVerify] = useState(false)
  const [verifyProgress, setVerifyProgress] = useState({ current: 0, total: 0 })
  const [verifyResults, setVerifyResults] = useState<Map<number, VerifyResult>>(new Map())
  const [balanceMap, setBalanceMap] = useState<Map<number, BalanceResponse>>(new Map())
  const [loadingBalanceIds, setLoadingBalanceIds] = useState<Set<number>>(new Set())
  const [queryingInfo, setQueryingInfo] = useState(false)
  const [queryInfoProgress, setQueryInfoProgress] = useState({ current: 0, total: 0 })
  const [batchRefreshing, setBatchRefreshing] = useState(false)
  const [batchRefreshProgress, setBatchRefreshProgress] = useState({ current: 0, total: 0 })
  const cancelVerifyRef = useRef(false)
  const [currentPage, setCurrentPage] = useState(1)
  const itemsPerPage = 12
  const [darkMode, setDarkMode] = useState(() => {
    if (typeof window !== 'undefined') {
      return document.documentElement.classList.contains('dark')
    }
    return false
  })

  const queryClient = useQueryClient()
  const { data, isLoading, error, refetch } = useCredentials()
  const { mutate: deleteCredential } = useDeleteCredential()
  const { mutate: resetFailure } = useResetFailure()

  // 计算分页
  const totalPages = Math.ceil((data?.credentials.length || 0) / itemsPerPage)
  const startIndex = (currentPage - 1) * itemsPerPage
  const endIndex = startIndex + itemsPerPage
  const currentCredentials = data?.credentials.slice(startIndex, endIndex) || []
  const disabledCredentialCount = data?.credentials.filter(credential => credential.disabled).length || 0
  const selectedDisabledCount = Array.from(selectedIds).filter(id => {
    const credential = data?.credentials.find(c => c.id === id)
    return Boolean(credential?.disabled)
  }).length

  // 当凭据列表变化时重置到第一页
  useEffect(() => {
    setCurrentPage(1)
  }, [data?.credentials.length])

  // 只保留当前仍存在的凭据缓存，避免删除后残留旧数据
  useEffect(() => {
    if (!data?.credentials) {
      setBalanceMap(new Map())
      setLoadingBalanceIds(new Set())
      return
    }

    const validIds = new Set(data.credentials.map(credential => credential.id))

    setBalanceMap(prev => {
      const next = new Map<number, BalanceResponse>()
      prev.forEach((value, id) => {
        if (validIds.has(id)) {
          next.set(id, value)
        }
      })
      return next.size === prev.size ? prev : next
    })

    setLoadingBalanceIds(prev => {
      if (prev.size === 0) {
        return prev
      }
      const next = new Set<number>()
      prev.forEach(id => {
        if (validIds.has(id)) {
          next.add(id)
        }
      })
      return next.size === prev.size ? prev : next
    })
  }, [data?.credentials])

  const toggleDarkMode = () => {
    setDarkMode(!darkMode)
    document.documentElement.classList.toggle('dark')
  }

  const handleViewBalance = (id: number) => {
    setSelectedCredentialId(id)
    setBalanceDialogOpen(true)
  }

  const handleRefresh = () => {
    refetch()
    toast.success('已刷新凭据列表')
  }

  const handleLogout = () => {
    storage.removeApiKey()
    queryClient.clear()
    onLogout()
  }

  // 选择管理
  const toggleSelect = (id: number) => {
    const newSelected = new Set(selectedIds)
    if (newSelected.has(id)) {
      newSelected.delete(id)
    } else {
      newSelected.add(id)
    }
    setSelectedIds(newSelected)
  }

  const deselectAll = () => {
    setSelectedIds(new Set())
  }

  // 当前页全选 / 取消全选（不影响其他页已选中的项）
  const toggleSelectAllOnPage = () => {
    const pageIds = currentCredentials.map(c => c.id)
    const allOnPageSelected = pageIds.length > 0 && pageIds.every(id => selectedIds.has(id))
    const newSelected = new Set(selectedIds)
    if (allOnPageSelected) {
      pageIds.forEach(id => newSelected.delete(id))
    } else {
      pageIds.forEach(id => newSelected.add(id))
    }
    setSelectedIds(newSelected)
  }

  // 当前页选择状态：false / true / "indeterminate"
  const pageSelectionState: boolean | 'indeterminate' = (() => {
    if (currentCredentials.length === 0) return false
    const count = currentCredentials.filter(c => selectedIds.has(c.id)).length
    if (count === 0) return false
    if (count === currentCredentials.length) return true
    return 'indeterminate'
  })()

  // 批量删除（仅删除已禁用项）
  const handleBatchDelete = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要删除的凭据')
      return
    }

    const disabledIds = Array.from(selectedIds).filter(id => {
      const credential = data?.credentials.find(c => c.id === id)
      return Boolean(credential?.disabled)
    })

    if (disabledIds.length === 0) {
      toast.error('选中的凭据中没有已禁用项')
      return
    }

    const skippedCount = selectedIds.size - disabledIds.length
    const skippedText = skippedCount > 0 ? `（将跳过 ${skippedCount} 个未禁用凭据）` : ''

    if (!confirm(`确定要删除 ${disabledIds.length} 个已禁用凭据吗？此操作无法撤销。${skippedText}`)) {
      return
    }

    let successCount = 0
    let failCount = 0

    for (const id of disabledIds) {
      try {
        await new Promise<void>((resolve, reject) => {
          deleteCredential(id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    const skippedResultText = skippedCount > 0 ? `，已跳过 ${skippedCount} 个未禁用凭据` : ''

    if (failCount === 0) {
      toast.success(`成功删除 ${successCount} 个已禁用凭据${skippedResultText}`)
    } else {
      toast.warning(`删除已禁用凭据：成功 ${successCount} 个，失败 ${failCount} 个${skippedResultText}`)
    }

    deselectAll()
  }

  // 批量恢复异常
  const handleBatchResetFailure = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要恢复的凭据')
      return
    }

    const failedIds = Array.from(selectedIds).filter(id => {
      const cred = data?.credentials.find(c => c.id === id)
      return cred && cred.failureCount > 0
    })

    if (failedIds.length === 0) {
      toast.error('选中的凭据中没有失败的凭据')
      return
    }

    let successCount = 0
    let failCount = 0

    for (const id of failedIds) {
      try {
        await new Promise<void>((resolve, reject) => {
          resetFailure(id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    if (failCount === 0) {
      toast.success(`成功恢复 ${successCount} 个凭据`)
    } else {
      toast.warning(`成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  // 批量刷新 Token
  const handleBatchForceRefresh = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要刷新的凭据')
      return
    }

    const enabledIds = Array.from(selectedIds).filter(id => {
      const cred = data?.credentials.find(c => c.id === id)
      return cred && !cred.disabled
    })

    if (enabledIds.length === 0) {
      toast.error('选中的凭据中没有启用的凭据')
      return
    }

    setBatchRefreshing(true)
    setBatchRefreshProgress({ current: 0, total: enabledIds.length })

    let successCount = 0
    let failCount = 0

    for (let i = 0; i < enabledIds.length; i++) {
      try {
        await forceRefreshToken(enabledIds[i])
        successCount++
      } catch {
        failCount++
      }
      setBatchRefreshProgress({ current: i + 1, total: enabledIds.length })
    }

    setBatchRefreshing(false)
    queryClient.invalidateQueries({ queryKey: ['credentials'] })

    if (failCount === 0) {
      toast.success(`成功刷新 ${successCount} 个凭据的 Token`)
    } else {
      toast.warning(`刷新 Token：成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  // 一键清除所有已禁用凭据
  const handleClearAll = async () => {
    if (!data?.credentials || data.credentials.length === 0) {
      toast.error('没有可清除的凭据')
      return
    }

    const disabledCredentials = data.credentials.filter(credential => credential.disabled)

    if (disabledCredentials.length === 0) {
      toast.error('没有可清除的已禁用凭据')
      return
    }

    if (!confirm(`确定要清除所有 ${disabledCredentials.length} 个已禁用凭据吗？此操作无法撤销。`)) {
      return
    }

    let successCount = 0
    let failCount = 0

    for (const credential of disabledCredentials) {
      try {
        await new Promise<void>((resolve, reject) => {
          deleteCredential(credential.id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    if (failCount === 0) {
      toast.success(`成功清除所有 ${successCount} 个已禁用凭据`)
    } else {
      toast.warning(`清除已禁用凭据：成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  // 查询当前页凭据信息（逐个查询，避免瞬时并发）
  const handleQueryCurrentPageInfo = async () => {
    if (currentCredentials.length === 0) {
      toast.error('当前页没有可查询的凭据')
      return
    }

    const ids = currentCredentials
      .filter(credential => !credential.disabled)
      .map(credential => credential.id)

    if (ids.length === 0) {
      toast.error('当前页没有可查询的启用凭据')
      return
    }

    setQueryingInfo(true)
    setQueryInfoProgress({ current: 0, total: ids.length })

    let successCount = 0
    let failCount = 0

    for (let i = 0; i < ids.length; i++) {
      const id = ids[i]

      setLoadingBalanceIds(prev => {
        const next = new Set(prev)
        next.add(id)
        return next
      })

      try {
        const balance = await getCredentialBalance(id)
        successCount++

        setBalanceMap(prev => {
          const next = new Map(prev)
          next.set(id, balance)
          return next
        })
      } catch (error) {
        failCount++
      } finally {
        setLoadingBalanceIds(prev => {
          const next = new Set(prev)
          next.delete(id)
          return next
        })
      }

      setQueryInfoProgress({ current: i + 1, total: ids.length })
    }

    setQueryingInfo(false)

    if (failCount === 0) {
      toast.success(`查询完成：成功 ${successCount}/${ids.length}`)
    } else {
      toast.warning(`查询完成：成功 ${successCount} 个，失败 ${failCount} 个`)
    }
  }

  // 批量验活
  // 打开批量验证对话框（不立即开跑，先让用户在 dialog 里选模型）
  const handleOpenBatchVerify = () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要验证的凭据')
      return
    }
    setVerifyResults(new Map())
    setHasStartedVerify(false)
    setVerifyDialogOpen(true)
  }

  // 用 verifyCredentialMessage 对所有选中凭据真实发一次 messages 请求
  const handleStartBatchVerify = async (model: string) => {
    const ids = Array.from(selectedIds)
    if (ids.length === 0) return

    setHasStartedVerify(true)
    setVerifying(true)
    cancelVerifyRef.current = false
    setVerifyProgress({ current: 0, total: ids.length })

    const initialResults = new Map<number, VerifyResult>()
    ids.forEach(id => {
      initialResults.set(id, { id, status: 'pending' })
    })
    setVerifyResults(initialResults)

    let successCount = 0

    for (let i = 0; i < ids.length; i++) {
      if (cancelVerifyRef.current) {
        toast.info('已取消验证')
        break
      }

      const id = ids[i]

      setVerifyResults(prev => {
        const newResults = new Map(prev)
        newResults.set(id, { id, status: 'verifying' })
        return newResults
      })

      try {
        const res = await verifyCredentialMessage(id, model)
        if (res.ok) {
          successCount++
          setVerifyResults(prev => {
            const newResults = new Map(prev)
            newResults.set(id, {
              id,
              status: 'success',
              latencyMs: res.latencyMs,
              statusCode: res.status ?? undefined,
            })
            return newResults
          })
        } else {
          setVerifyResults(prev => {
            const newResults = new Map(prev)
            newResults.set(id, {
              id,
              status: 'failed',
              latencyMs: res.latencyMs,
              statusCode: res.status ?? undefined,
              error: res.error ?? '未知错误',
            })
            return newResults
          })
        }
      } catch (error) {
        setVerifyResults(prev => {
          const newResults = new Map(prev)
          newResults.set(id, {
            id,
            status: 'failed',
            error: extractErrorMessage(error),
          })
          return newResults
        })
      }

      setVerifyProgress({ current: i + 1, total: ids.length })

      // 间隔 2 秒避免被风控判定为可疑活动
      if (i < ids.length - 1 && !cancelVerifyRef.current) {
        await new Promise(resolve => setTimeout(resolve, 2000))
      }
    }

    setVerifying(false)

    if (!cancelVerifyRef.current) {
      toast.success(`验证完成：成功 ${successCount}/${ids.length}`)
    }
  }

  const handleCancelVerify = () => {
    cancelVerifyRef.current = true
    setVerifying(false)
  }

  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto mb-4"></div>
          <p className="text-muted-foreground">加载中...</p>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background p-4">
        <Card className="w-full max-w-md">
          <CardContent className="pt-6 text-center">
            <div className="text-red-500 mb-4">加载失败</div>
            <p className="text-muted-foreground mb-4">{(error as Error).message}</p>
            <div className="space-x-2">
              <Button onClick={() => refetch()}>重试</Button>
              <Button variant="outline" onClick={handleLogout}>重新登录</Button>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="min-h-screen bg-background">
      {/* 顶部导航 */}
      <header className="sticky top-0 z-50 w-full border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="container flex h-14 items-center justify-between px-4 md:px-8">
          <div className="flex items-center gap-2">
            <Server className="h-5 w-5" />
            <span className="font-semibold">Kiro Admin</span>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="icon"
              onClick={() => setUsageSettingsOpen(true)}
              title="usage 上报设置"
            >
              <SlidersHorizontal className="h-5 w-5" />
            </Button>
            <Button variant="ghost" size="icon" onClick={toggleDarkMode}>
              {darkMode ? <Sun className="h-5 w-5" /> : <Moon className="h-5 w-5" />}
            </Button>
            <Button variant="ghost" size="icon" onClick={handleRefresh} title="刷新凭据列表">
              <RefreshCw className="h-5 w-5" />
            </Button>
            <Button variant="ghost" size="icon" onClick={handleLogout}>
              <LogOut className="h-5 w-5" />
            </Button>
          </div>
        </div>
      </header>

      {/* 主内容 */}
      <main className="container mx-auto px-4 md:px-8 py-6">
        {/* 统计卡片 */}
        <div className="grid gap-4 md:grid-cols-3 mb-6">
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                凭据总数
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{data?.total || 0}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                可用凭据
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-green-600">{data?.available || 0}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                当前活跃
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold flex items-center gap-2">
                #{data?.currentId || '-'}
                <Badge variant="success">活跃</Badge>
              </div>
            </CardContent>
          </Card>
        </div>

        {/* 凭据列表 */}
        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-4">
              <h2 className="text-xl font-semibold">凭据管理</h2>
              {selectedIds.size > 0 && (
                <div className="flex items-center gap-2">
                  <Badge variant="secondary">已选择 {selectedIds.size} 个</Badge>
                  <Button onClick={deselectAll} size="sm" variant="ghost">
                    取消选择
                  </Button>
                </div>
              )}
            </div>
            <div className="flex gap-2">
              {selectedIds.size > 0 && (
                <>
                  <Button onClick={handleOpenBatchVerify} size="sm" variant="outline">
                    <PlayCircle className="h-4 w-4 mr-2" />
                    批量验证
                  </Button>
                  <Button
                    onClick={handleBatchForceRefresh}
                    size="sm"
                    variant="outline"
                    disabled={batchRefreshing}
                  >
                    <RefreshCw className={`h-4 w-4 mr-2 ${batchRefreshing ? 'animate-spin' : ''}`} />
                    {batchRefreshing ? `刷新中... ${batchRefreshProgress.current}/${batchRefreshProgress.total}` : '批量刷新 Token'}
                  </Button>
                  <Button onClick={handleBatchResetFailure} size="sm" variant="outline">
                    <RotateCcw className="h-4 w-4 mr-2" />
                    重置失败
                  </Button>
                  <Button
                    onClick={handleBatchDelete}
                    size="sm"
                    variant="destructive"
                    disabled={selectedDisabledCount === 0}
                    title={selectedDisabledCount === 0 ? '只能删除已禁用凭据' : undefined}
                  >
                    <Trash2 className="h-4 w-4 mr-2" />
                    批量删除
                  </Button>
                </>
              )}
              {verifying && !verifyDialogOpen && (
                <Button onClick={() => setVerifyDialogOpen(true)} size="sm" variant="secondary">
                  <PlayCircle className="h-4 w-4 mr-2 animate-spin" />
                  验证中... {verifyProgress.current}/{verifyProgress.total}
                </Button>
              )}
              {data?.credentials && data.credentials.length > 0 && (
                <Button
                  onClick={handleQueryCurrentPageInfo}
                  size="sm"
                  variant="outline"
                  disabled={queryingInfo}
                  title="批量拉取当前页所有启用凭据的余额信息"
                >
                  <Wallet className={`h-4 w-4 mr-2 ${queryingInfo ? 'animate-pulse' : ''}`} />
                  {queryingInfo ? `刷新中... ${queryInfoProgress.current}/${queryInfoProgress.total}` : '刷新余额'}
                </Button>
              )}
              {data?.credentials && data.credentials.length > 0 && (
                <Button
                  onClick={handleClearAll}
                  size="sm"
                  variant="outline"
                  className="text-destructive hover:text-destructive"
                  disabled={disabledCredentialCount === 0}
                  title={disabledCredentialCount === 0 ? '没有可清除的已禁用凭据' : undefined}
                >
                  <Trash2 className="h-4 w-4 mr-2" />
                  清除已禁用
                </Button>
              )}
              <Button onClick={() => setKamImportDialogOpen(true)} size="sm" variant="outline">
                <FileUp className="h-4 w-4 mr-2" />
                Kiro Account Manager 导入
              </Button>
              <Button onClick={() => setBatchImportDialogOpen(true)} size="sm" variant="outline">
                <Upload className="h-4 w-4 mr-2" />
                批量导入
              </Button>
              <Button onClick={() => { setEditTarget(null); setAddDialogOpen(true) }} size="sm">
                <Plus className="h-4 w-4 mr-2" />
                添加凭据
              </Button>
            </div>
          </div>
          {data?.credentials.length === 0 ? (
            <Card>
              <CardContent className="py-8 text-center text-muted-foreground">
                暂无凭据
              </CardContent>
            </Card>
          ) : (
            <>
              <div className="rounded-md border bg-card overflow-x-auto">
                <table className="w-full text-sm">
                  <thead className="bg-muted/50 text-xs text-muted-foreground">
                    <tr className="border-b">
                      <th className="px-2 py-2 text-left font-medium w-8">
                        <Checkbox
                          checked={pageSelectionState}
                          onCheckedChange={toggleSelectAllOnPage}
                          aria-label="选择当前页全部凭据"
                        />
                      </th>
                      <th className="px-2 py-2 text-left font-medium">凭据</th>
                      <th className="px-2 py-2 text-left font-medium">订阅</th>
                      <th className="px-2 py-2 text-left font-medium">优先级</th>
                      <th className="px-2 py-2 text-left font-medium">状态</th>
                      <th className="px-2 py-2 text-left font-medium">并发</th>
                      <th className="px-2 py-2 text-left font-medium">计数 / 用量</th>
                      <th className="px-2 py-2 text-left font-medium">消耗金额</th>
                      <th className="px-2 py-2 text-left font-medium">最近使用</th>
                      <th className="px-2 py-2 text-left font-medium">验证</th>
                      <th className="px-2 py-2 text-left font-medium">操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {currentCredentials.map((credential) => (
                      <CredentialRow
                        key={credential.id}
                        credential={credential}
                        onViewBalance={handleViewBalance}
                        onEdit={(cred) => {
                          setEditTarget(cred)
                          setAddDialogOpen(true)
                        }}
                        selected={selectedIds.has(credential.id)}
                        onToggleSelect={() => toggleSelect(credential.id)}
                        balance={balanceMap.get(credential.id) || null}
                        loadingBalance={loadingBalanceIds.has(credential.id)}
                      />
                    ))}
                  </tbody>
                </table>
              </div>

              {/* 分页控件 */}
              {totalPages > 1 && (
                <div className="flex justify-center items-center gap-4 mt-6">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setCurrentPage(p => Math.max(1, p - 1))}
                    disabled={currentPage === 1}
                  >
                    上一页
                  </Button>
                  <span className="text-sm text-muted-foreground">
                    第 {currentPage} / {totalPages} 页（共 {data?.credentials.length} 个凭据）
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setCurrentPage(p => Math.min(totalPages, p + 1))}
                    disabled={currentPage === totalPages}
                  >
                    下一页
                  </Button>
                </div>
              )}
            </>
          )}
        </div>
      </main>

      {/* 余额对话框 */}
      <BalanceDialog
        credentialId={selectedCredentialId}
        open={balanceDialogOpen}
        onOpenChange={setBalanceDialogOpen}
      />

      {/* 添加 / 编辑凭据对话框（复用一个组件） */}
      <AddCredentialDialog
        open={addDialogOpen}
        onOpenChange={(open) => {
          setAddDialogOpen(open)
          if (!open) setEditTarget(null)
        }}
        mode={editTarget ? 'edit' : 'add'}
        editTarget={editTarget}
      />

      {/* 批量导入对话框 */}
      <BatchImportDialog
        open={batchImportDialogOpen}
        onOpenChange={setBatchImportDialogOpen}
      />

      {/* KAM 账号导入对话框 */}
      <KamImportDialog
        open={kamImportDialogOpen}
        onOpenChange={setKamImportDialogOpen}
      />

      {/* usage 上报设置对话框 */}
      <UsageSettingsDialog
        open={usageSettingsOpen}
        onOpenChange={setUsageSettingsOpen}
      />

      {/* 批量验证对话框 */}
      <BatchVerifyDialog
        open={verifyDialogOpen}
        onOpenChange={setVerifyDialogOpen}
        hasStarted={hasStartedVerify}
        verifying={verifying}
        progress={verifyProgress}
        results={verifyResults}
        selectedCount={selectedIds.size}
        onStart={handleStartBatchVerify}
        onCancel={handleCancelVerify}
      />
    </div>
  )
}
