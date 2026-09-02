import { api } from './client'
import type {
  UsageCacheSettings,
  UpdateUsageCacheSettingsRequest,
} from '@/types/api'

// 获取 usage 上报设置
export async function getUsageCacheSettings(): Promise<UsageCacheSettings> {
  const { data } = await api.get<UsageCacheSettings>('/settings/usage-cache')
  return data
}

// 更新 usage 上报设置（热生效 + 写回 config.json）
export async function updateUsageCacheSettings(
  req: UpdateUsageCacheSettingsRequest
): Promise<UsageCacheSettings> {
  const { data } = await api.patch<UsageCacheSettings>(
    '/settings/usage-cache',
    req
  )
  return data
}
