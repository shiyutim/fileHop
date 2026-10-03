<script setup lang="ts">
import { computed, ref } from 'vue';
import AppIcon from './AppIcon.vue';
import { formatBytes, isTerminal, type Transfer, type TransferStatus } from '../types';
const props = defineProps<{ transfer: Transfer; speed?: number; busy?: boolean }>();
defineEmits<{ respond: [id: string, accept: boolean, trust?: boolean]; cancel: [id: string] }>();
const confirmed = ref(false);
const trustDevice = ref(false);
const trustedConnection = computed(() => props.transfer.peerTrusted && props.transfer.localConfirmed);
const hasFolders = computed(() => props.transfer.files.some((file) => file.isDirectory));
const percentage = computed(() =>
  props.transfer.status === 'completed'
    ? 100
    : Math.min(
        100,
        Math.round(
          (props.transfer.transferredBytes / Math.max(1, props.transfer.totalBytes)) * 100,
        ),
      ),
);
const labels: Record<TransferStatus, string> = {
  preparing: '正在压缩',
  connecting: '正在连接',
  waiting: '等待确认',
  transferring: '正在传输',
  completed: '已完成',
  rejected: '已拒绝',
  cancelled: '已取消',
  failed: '传输失败',
};
const codeGroups = computed(() => props.transfer.verificationCode?.match(/.{1,4}/g) || []);
const time = computed(() =>
  new Date(props.transfer.createdAt).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }),
);
const remaining = computed(() => {
  if (!props.speed || props.speed < 1) return '';
  const seconds = Math.ceil(
    (props.transfer.totalBytes - props.transfer.transferredBytes) / props.speed,
  );
  return seconds > 60 ? `约 ${Math.ceil(seconds / 60)} 分钟` : `约 ${Math.max(1, seconds)} 秒`;
});
</script>
<template>
  <article class="transfer-card" :class="{ 'needs-confirmation': transfer.status === 'waiting' }">
    <div class="transfer-top">
      <span class="transfer-direction" :class="transfer.direction"
        ><AppIcon :name="transfer.direction === 'send' ? 'arrowUp' : 'arrowDown'"
      /></span>
      <div class="transfer-title">
        <strong :title="transfer.files.map((file) => file.name).join('、')"
          >{{ transfer.files[0]?.name || '文件传输'
          }}<span v-if="transfer.files.length > 1" class="file-count"
            >等 {{ transfer.files.length }} {{ hasFolders ? '项' : '个文件' }}</span
          ></strong
        ><span
          >{{ transfer.direction === 'send' ? '发送给' : '来自' }} {{ transfer.peerName
          }}<span class="middot">·</span>{{ hasFolders ? '大小待压缩' : formatBytes(transfer.totalBytes)
          }}<span v-if="isTerminal(transfer.status)" class="transfer-time">{{ time }}</span></span
        >
      </div>
      <span class="transfer-status" :class="transfer.status"
        ><AppIcon v-if="transfer.status === 'completed'" name="check" :size="14" />{{
          labels[transfer.status]
        }}</span
      ><button
        v-if="!isTerminal(transfer.status) && transfer.status !== 'waiting'"
        class="icon-button"
        :disabled="busy"
        aria-label="取消传输"
        title="取消传输"
        @click="$emit('cancel', transfer.id)"
      >
        <AppIcon name="close" :size="17" />
      </button>
    </div>
    <div v-if="transfer.status === 'preparing'" class="preparing-details">
      <div class="progress-track preparing-progress" role="progressbar" aria-label="正在将文件夹压缩为 ZIP">
        <div />
      </div>
    </div>
    <div v-if="transfer.status === 'waiting'" class="verification">
      <div class="verification-heading">
        <AppIcon name="shield" :size="17" /><span>{{
          trustedConnection ? '受信任设备' : '核对两台电脑的完整校验码'
        }}</span>
      </div>
      <div v-if="codeGroups.length" class="verification-code" aria-label="完整连接校验码">
        <span v-for="(group, index) in codeGroups" :key="index">{{ group }}</span>
      </div>
      <p v-else>正在生成连接校验码…</p>
      <div v-if="transfer.files.length > 1" class="request-file-list">
        <span v-for="(file, index) in transfer.files.slice(0, 5)" :key="index"
          ><AppIcon name="file" :size="13" />{{ file.name
          }}<small>{{ formatBytes(file.size) }}</small></span
        ><small v-if="transfer.files.length > 5">另外 {{ transfer.files.length - 5 }} 个文件</small>
      </div>
      <p
        v-if="transfer.direction === 'receive' && transfer.saveDir"
        class="request-save-dir"
        :title="transfer.saveDir"
      >
        保存到：{{ transfer.saveDir }}
      </p>
      <template v-if="!transfer.localConfirmed"
        ><label class="verification-checkbox"
          ><input v-model="confirmed" type="checkbox" :disabled="busy" /><span
            >两台电脑的完整校验码一致</span
          ></label
        >
        <label class="verification-checkbox trust-checkbox"
          ><input
            v-model="trustDevice"
            type="checkbox"
            :disabled="busy"
            :aria-describedby="`trust-help-${transfer.id}`"
          /><span>长期信任此设备</span></label
        >
        <p :id="`trust-help-${transfer.id}`" class="trust-help">
          以后自动确认并接收此设备的文件，可在偏好设置中取消信任。
        </p>
        <div class="verification-actions">
          <button
            class="button button-primary button-small"
            :disabled="!confirmed || !transfer.verificationCode || busy"
            @click="$emit('respond', transfer.id, true, trustDevice)"
          >
            {{ transfer.direction === 'receive' ? '确认接收' : '确认连接'
            }}<AppIcon name="check" :size="16" /></button
          ><button
            class="button button-quiet button-small"
            :disabled="busy"
            @click="
              transfer.direction === 'receive'
                ? $emit('respond', transfer.id, false)
                : $emit('cancel', transfer.id)
            "
          >
            {{ transfer.direction === 'receive' ? '拒绝' : '取消' }}
          </button>
        </div></template
      >
      <div v-else class="confirmed-row">
        <span><AppIcon name="check" :size="16" />{{
          trustedConnection ? '本机已自动确认，等待对方确认' : '本机已确认，等待对方确认'
        }}</span
        ><button class="text-button" :disabled="busy" @click="$emit('cancel', transfer.id)">
          取消
        </button>
      </div>
    </div>
    <template v-if="transfer.status === 'transferring'"
      ><div
        class="progress-track"
        role="progressbar"
        :aria-valuenow="percentage"
        aria-valuemin="0"
        aria-valuemax="100"
        :aria-label="`${transfer.files[0]?.name || '文件'}传输进度`"
      >
        <div :style="{ width: `${percentage}%` }" />
      </div>
      <div class="progress-details">
        <span
          >{{ formatBytes(transfer.transferredBytes) }} /
          {{ formatBytes(transfer.totalBytes) }}</span
        ><span v-if="speed"
          >{{ formatBytes(speed) }}/s <span class="middot">·</span> {{ remaining }}</span
        ><strong>{{ percentage }}%</strong>
      </div></template
    >
    <p v-if="transfer.error" class="transfer-error">
      <AppIcon name="info" :size="15" />{{ transfer.error }}
    </p>
    <p
      v-if="transfer.status === 'completed' && transfer.direction === 'receive' && transfer.saveDir"
      class="received-path"
      :title="transfer.saveDir"
    >
      <AppIcon name="folder" :size="14" />{{ transfer.saveDir }}
    </p>
  </article>
</template>
