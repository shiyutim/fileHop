export interface FileItem {
  name: string;
  size: number;
  path?: string;
  isDirectory?: boolean;
}
export interface Device {
  id: string;
  name: string;
  platform: string;
  addresses: string[];
  port: number;
}
export interface Peer {
  id: string;
  name: string;
  platform: string;
  address: string;
  port: number;
  lastSeen: number;
}
export interface TrustedDevice {
  publicKey: string;
  name: string;
  trustedAt: number;
}
export type TransferStatus =
  'preparing' | 'connecting' | 'waiting' | 'transferring' | 'completed' | 'rejected' | 'cancelled' | 'failed';
export interface Transfer {
  id: string;
  direction: 'send' | 'receive';
  peerName: string;
  files: FileItem[];
  totalBytes: number;
  transferredBytes: number;
  status: TransferStatus;
  verificationCode?: string;
  localConfirmed?: boolean;
  peerTrusted?: boolean;
  error?: string;
  createdAt: number;
  completedAt?: number;
  saveDir?: string;
}
export interface Snapshot {
  device: Device;
  peers: Peer[];
  transfers: Transfer[];
  trustedDevices: TrustedDevice[];
  saveDir: string;
  discoveryError?: string;
  serverError?: string;
}
export const isTerminal = (status: TransferStatus) =>
  ['completed', 'rejected', 'cancelled', 'failed'].includes(status);
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const amount = bytes / 1024 ** index;
  return `${amount.toFixed(index === 0 || amount >= 100 ? 0 : 1)} ${units[index]}`;
}
export function platformName(platform: string): string {
  const value = platform.toLowerCase();
  if (value.includes('mac') || value === 'darwin') return 'macOS';
  if (value.includes('win')) return 'Windows';
  if (value.includes('linux')) return 'Linux';
  return platform || '桌面设备';
}
export const endpoint = (address: string, port: number) =>
  `${address.includes(':') && !address.startsWith('[') ? `[${address}]` : address}:${port}`;
