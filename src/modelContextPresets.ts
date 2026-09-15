export const MIN_CONTEXT_WINDOW_TOKENS = 1024;
export const MAX_CONTEXT_WINDOW_TOKENS = 10_000_000;
export const DEFAULT_CONTEXT_WINDOW_TOKENS = 200_000;

/// 常见的模型上下文窗口，供选择器直接选取；也可以自行输入其它数值。
export const CONTEXT_WINDOW_PRESETS: ReadonlyArray<{
  label: string;
  value: number;
}> = [
  { label: "32K", value: 32_000 },
  { label: "64K", value: 64_000 },
  { label: "128K", value: 128_000 },
  { label: "200K", value: 200_000 },
  { label: "256K", value: 256_000 },
  { label: "400K", value: 400_000 },
  { label: "512K", value: 512_000 },
  { label: "1M", value: 1_000_000 },
];
