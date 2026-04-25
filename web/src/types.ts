export interface Workspace {
  name: string;
}

export interface Bot {
  name: string;
  color?: string;
  role?: string;
  watch: string[];
}

export interface Worker {
  id: string;
  branch: string;
  status: string;
  agent: string;
  pr_url: string | null;
  pr_title: string | null;
  description: string | null;
  elapsed_secs: number | null;
  dispatched_by: string | null;
}

export interface Message {
  id: number;
  workspace: string;
  bot: string;
  role: string;
  content: string;
  attachments: string | null;
  created_at: string;
}
