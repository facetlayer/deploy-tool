import { createContext, useContext, useEffect, useState } from "react";
import { api, setUnauthorizedHandler, Me } from "./api";
import { useRoute } from "./util";
import { Projects } from "./pages/Projects";
import { ProjectDetail } from "./pages/ProjectDetail";

const SessionCtx = createContext<Me | null>(null);
export const useSession = () => useContext(SessionCtx)!;

export function App() {
  const [me, setMe] = useState<Me | null | undefined>(undefined);

  useEffect(() => {
    setUnauthorizedHandler(() => setMe(null));
    api.get<Me>("/me").then(setMe).catch(() => setMe(null));
  }, []);

  if (me === undefined) return <div className="login"><div className="muted mono">…</div></div>;
  if (me === null) return <SignIn />;
  return (
    <SessionCtx.Provider value={me}>
      <Shell />
    </SessionCtx.Provider>
  );
}

/** There is no password form here. The only way in is auth-center, so this
 *  screen is a door rather than a login. */
function SignIn() {
  return (
    <div className="login">
      <div className="login-card">
        <div className="brand">deploy</div>
        <h1>Sign in</h1>
        <p className="muted">
          This dashboard uses your auth-center account. You will be sent there
          and back again.
        </p>
        <p style={{ marginTop: 22 }}>
          <a className="btn btn-primary" href="/oauth/login" style={{ display: "inline-block" }}>
            Continue to auth-center
          </a>
        </p>
      </div>
    </div>
  );
}

function Shell() {
  const me = useSession();
  const route = useRoute();
  const section = route[0] ?? "projects";

  const signOut = () => {
    // Drop the local session first, then hand off to auth-center's logout so
    // the session there ends too. Doing it in the other order would leave this
    // server's cookie alive if the redirect were interrupted.
    api.post("/logout").catch(() => {}).finally(() => {
      window.location.href = me.logoutUrl || "/";
    });
  };

  return (
    <div className="shell">
      <aside className="side">
        <div className="brand">deploy</div>
        <nav className="nav">
          <a className={section === "projects" ? "active" : ""} href="#/projects">Projects</a>
        </nav>
        <div className="side-foot">
          <div className="mono">{me.username}</div>
          <button className="btn btn-sm" style={{ marginTop: 8 }} onClick={signOut}>Sign out</button>
        </div>
      </aside>
      <main className="main">
        {route[0] === "projects" && route[1]
          ? <ProjectDetail name={route[1]} />
          : <Projects />}
      </main>
    </div>
  );
}
