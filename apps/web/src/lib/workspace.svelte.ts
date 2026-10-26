import { listConnectors, listProjects, listSessions } from './api/workspace';
import type { Connector, Project, Session } from './api/types';
import { describeError } from './format';

export type ProjectGroup = { project: Project | null; sessions: Session[] };
export type DeviceGroup = { connector: Connector; projectGroups: ProjectGroup[]; sessionCount: number };

function activityRank(session: Session): number {
	const timestamp = session.lastActivityAt ? Date.parse(session.lastActivityAt) : Number.NaN;
	return Number.isNaN(timestamp) ? 0 : timestamp;
}

class WorkspaceStore {
	connectors = $state<Connector[]>([]);
	projects = $state<Project[]>([]);
	sessions = $state<Session[]>([]);
	loading = $state(false);
	error = $state<string | null>(null);
	loadedAt = $state<number>(0);

	get groups(): DeviceGroup[] {
		const projectsById = new Map(this.projects.map((project) => [project.id, project]));
		const byConnector = new Map<string, Session[]>();
		for (const session of this.sessions) {
			const bucket = byConnector.get(session.connectorId);
			if (bucket) bucket.push(session);
			else byConnector.set(session.connectorId, [session]);
		}
		return this.connectors.map((connector) => {
			const rows = (byConnector.get(connector.id) ?? []).sort((a, b) => activityRank(b) - activityRank(a));
			const projectGroups: ProjectGroup[] = [];
			const orphans: Session[] = [];
			const byProject = new Map<string, Session[]>();
			for (const session of rows) {
				const project = session.projectId ? projectsById.get(session.projectId) : null;
				if (!project) {
					orphans.push(session);
					continue;
				}
				const bucket = byProject.get(project.id);
				if (bucket) bucket.push(session);
				else byProject.set(project.id, [session]);
			}
			for (const project of this.projects.filter((item) => item.connectorId === connector.id)) {
				const sessions = byProject.get(project.id) ?? [];
				if (sessions.length === 0) continue;
				projectGroups.push({ project, sessions });
			}
			if (orphans.length > 0) projectGroups.push({ project: null, sessions: orphans });
			return { connector, projectGroups, sessionCount: rows.length };
		});
	}

	get onlineDevices(): number {
		return this.connectors.filter((connector) => connector.status === 'online').length;
	}

	get activeSessionCount(): number {
		return this.sessions.filter((session) => !session.archived).length;
	}

	sessionById(id: string): Session | null {
		return this.sessions.find((session) => session.id === id) ?? null;
	}

	projectById(id: string | null): Project | null {
		if (!id) return null;
		return this.projects.find((project) => project.id === id) ?? null;
	}

	connectorById(id: string | null): Connector | null {
		if (!id) return null;
		return this.connectors.find((connector) => connector.id === id) ?? null;
	}

	async load(token: string): Promise<void> {
		this.loading = true;
		this.error = null;
		try {
			const [connectors, projects, sessions] = await Promise.all([
				listConnectors(token),
				listProjects(token),
				listSessions(token)
			]);
			this.connectors = connectors;
			this.projects = projects;
			this.sessions = sessions;
			this.loadedAt = Date.now();
		} catch (error) {
			this.error = describeError(error);
		} finally {
			this.loading = false;
		}
	}
}

export const workspace = new WorkspaceStore();
