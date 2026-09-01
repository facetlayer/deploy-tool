import { api, ProjectDetail as Detail } from "../api";
import { Badge, Empty, ErrorBox, Link, Time, useAsync } from "../util";

export function ProjectDetail({ name }: { name: string }) {
  const detail = useAsync(() => api.get<Detail>(`/projects/${encodeURIComponent(name)}`), [name]);

  return (
    <>
      <div className="page-head">
        <div>
          <p style={{ margin: 0 }}><Link to="projects">← Projects</Link></p>
          <h1>{name}</h1>
        </div>
      </div>
      <ErrorBox error={detail.error} />
      {detail.data && (
        <>
          <dl className="kv">
            <dt>Resource</dt>
            <dd>
              {detail.data.project.resourceName
                ? <code className="scope">{detail.data.project.resourceName}</code>
                : <Badge kind="bad">unbound</Badge>}
            </dd>
            <dt>Registered</dt>
            <dd><Time at={detail.data.project.createdAt} exact /></dd>
            <dt>Live since</dt>
            <dd>{detail.data.project.activeSince ? <Time at={detail.data.project.activeSince} exact /> : <span className="muted">nothing live</span>}</dd>
          </dl>

          <h3>Deployment history</h3>
          {detail.data.deployments.length ? (
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th>Deployment</th>
                    <th>Created</th>
                    <th>Authorized by</th>
                    <th>Status</th>
                  </tr>
                </thead>
                <tbody>
                  {detail.data.deployments.map((d) => (
                    <tr key={d.deploy_name}>
                      <td className="mono small cell-wrap">{d.deploy_name}</td>
                      <td><Time at={d.created_at} /></td>
                      <td className="small">
                        {/* R7 attribution. Deployments made before it existed
                            have no key recorded, which is not the same as
                            nobody having made them. */}
                        {d.authorized_by_key_name ?? <span className="muted">unrecorded</span>}
                      </td>
                      <td>{d.is_active ? <Badge kind="ok">live</Badge> : <Badge kind="dim">superseded</Badge>}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : <Empty>Nothing has been deployed to this project.</Empty>}
        </>
      )}
    </>
  );
}
