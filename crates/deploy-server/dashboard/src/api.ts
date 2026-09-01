// Thin fetch wrapper over /dashboard/api. The session cookie does the auth; the
// browser never holds a token, because the server exchanged the OAuth code and
// kept it (see src/dashboard/oauth.rs for why).

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

/** Notified when the server says the session is gone — expiry, a sign-out
 *  elsewhere, or access withdrawn at auth-center. The app shows the sign-in
 *  prompt rather than painting "unauthorized" into every panel. */
type Unauthorized = () => void;
let onUnauthorized: Unauthorized = () => {};
export function setUnauthorizedHandler(fn: Unauthorized) {
  onUnauthorized = fn;
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const resp = await fetch(`/dashboard/api${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin",
  });
  const text = await resp.text();
  let data: any = null;
  try { data = text ? JSON.parse(text) : null; } catch { data = { error: text }; }
  if (!resp.ok) {
    if (resp.status === 401) onUnauthorized();
    throw new ApiError(resp.status, data?.error ?? resp.statusText);
  }
  return data as T;
}

export const api = {
  get: <T>(path: string) => call<T>("GET", path),
  post: <T>(path: string, body: unknown = {}) => call<T>("POST", path, body),
};

export interface Me {
  username: string;
  subject: string;
  /** auth-center's RP-initiated logout, so signing out here also ends the
   *  session there. Empty when the server has no dashboard config. */
  logoutUrl: string;
}

export interface ProjectSummary {
  projectName: string;
  createdAt: string;
  /** Absent when the project has never been bound to a resource — the state in
   *  which every call for it is denied, so it is shown rather than hidden. */
  resourceName?: string;
  activeDeployName?: string;
  activeSince?: string;
  lastDeployedAt?: string;
  deploymentCount: number;
  activeAuthorizedBy?: string;
}

/** Deliberately snake_case: these are the wire names the old CLI reads, and
 *  `DeploymentInfo` in deploy-core keeps them for that reason. */
export interface DeploymentInfo {
  deploy_name: string;
  created_at: string;
  is_active: boolean;
  authorized_by_key_id?: string;
  authorized_by_key_name?: string;
  tags?: Record<string, unknown>;
}

export interface ProjectDetail {
  project: ProjectSummary;
  deployments: DeploymentInfo[];
}
