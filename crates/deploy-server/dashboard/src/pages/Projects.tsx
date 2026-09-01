import { api, ProjectSummary } from "../api";
import { Badge, Empty, ErrorBox, Link, Time, useAsync } from "../util";

export function Projects() {
  const projects = useAsync(() => api.get<{ projects: ProjectSummary[] }>("/projects"), []);

  return (
    <>
      <div className="page-head">
        <div>
          <h1>Projects</h1>
          <p>Everything registered on this deploy server, and what is live right now.</p>
        </div>
      </div>
      <ErrorBox error={projects.error} />
      {projects.data && (projects.data.projects.length ? (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Project</th>
                <th>Resource</th>
                <th>Active deployment</th>
                <th>Live since</th>
                <th>Deployed by</th>
                <th className="num">Deployments</th>
              </tr>
            </thead>
            <tbody>
              {projects.data.projects.map((p) => (
                <tr key={p.projectName}>
                  <td><Link to={`projects/${encodeURIComponent(p.projectName)}`}><strong>{p.projectName}</strong></Link></td>
                  <td>
                    {p.resourceName
                      ? <code className="scope">{p.resourceName}</code>
                      // An unbound project is not a cosmetic gap: every call
                      // for it is denied until it is bound.
                      : <Badge kind="bad">unbound</Badge>}
                  </td>
                  <td className="mono small cell-wrap">
                    {p.activeDeployName ?? <span className="muted">nothing live</span>}
                  </td>
                  <td>{p.activeSince ? <Time at={p.activeSince} /> : <span className="muted">—</span>}</td>
                  <td className="small">{p.activeAuthorizedBy ?? <span className="muted">unrecorded</span>}</td>
                  <td className="num">{p.deploymentCount}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : <Empty>No projects are registered on this server yet.</Empty>)}
    </>
  );
}
