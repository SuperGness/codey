export type ReleaseChannel = "stable" | "experimental";
export type ReleaseIdentity = { version: string; tag: string; channel: ReleaseChannel; prerelease: boolean; name: string };
export function validateReleaseVersion(version: unknown): string;
export function releaseIdentity(version: string): ReleaseIdentity;
export function resolveReleaseVersion(version: string, channel?: string): ReleaseIdentity;
export function compareReleaseVersions(left: string, right: string): number;
