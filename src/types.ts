export type Lang = 'zh' | 'en';
export type ToastType = 'success' | 'error' | 'info';
export type SummaryMode = 'filename_only' | 'local_summary' | 'agent_summary';

export type JsonRecord = Record<string, unknown>;

export type CredentialsStatus = {
  providerHasApiKey?: Record<string, boolean>;
  searchApiHasKey?: boolean;
};

export type SearchApiSettings = {
  provider?: string;
  enabled?: boolean;
  scopes?: {
    classify?: boolean;
    organizer?: boolean;
  };
};

export type ProviderConfig = {
  name?: string;
  endpoint?: string;
  model?: string;
};

export type Settings = {
  providerConfigs?: Record<string, ProviderConfig>;
  defaultProviderEndpoint?: string;
  searchApi?: SearchApiSettings;
  credentialsStatus?: CredentialsStatus;
  storage?: {
    dataDir?: string;
    defaultDataDir?: string;
    customized?: boolean;
  };
};

export type CredentialsPayload = {
  providerSecrets?: Record<string, string>;
  searchApiKey?: string;
};

export type CredentialsReadResult = {
  providerSecrets?: Record<string, string>;
  searchApiKey?: string;
};

export type CredentialsSaveResult = {
  credentialsStatus?: CredentialsStatus;
};

export type ProviderRow = {
  name: string;
  endpoint: string;
  apiKey: string;
  model: string;
  modelLoaded?: boolean;
};

export type ProviderModelOption = {
  value: string;
  label: string;
};

export type BrowseFolderResult = {
  cancelled?: boolean;
  path?: string;
};

export type TreeNode = {
  id?: string;
  nodeId?: string;
  name?: string;
  itemCount?: number;
  children?: TreeNode[];
};

export type OrganizeResultRow = {
  index?: number;
  path?: string;
  name?: string;
  itemType?: string;
  categoryPath?: string[];
  leafNodeId?: string;
  classificationError?: string;
  reason?: string;
  [key: string]: unknown;
};

export type OrganizeSnapshot = {
  id?: string;
  status?: string;
  rootPath?: string;
  root_path?: string;
  totalFiles?: number;
  total_files?: number;
  processedFiles?: number;
  processed_files?: number;
  error?: string;
  summaryStrategy?: SummaryMode | string;
  summary_strategy?: SummaryMode | string;
  useWebSearch?: boolean;
  webSearchEnabled?: boolean;
  tree?: TreeNode;
  results?: OrganizeResultRow[];
  [key: string]: unknown;
};

export type AdvisorCardAction = {
  action?: string;
  label?: string;
  variant?: 'primary' | 'secondary' | 'danger' | string;
};

export type AdvisorCard = {
  cardId?: string;
  cardType?: string;
  title?: string;
  status?: string;
  createdAt?: string;
  body?: JsonRecord;
  actions?: AdvisorCardAction[];
};

export type AgentTraceStep = {
  step?: number | string;
  route?: JsonRecord;
  usage?: JsonRecord;
  assistantText?: string;
  toolCalls?: Array<{ id?: string; name?: string; arguments?: unknown }>;
  toolResults?: Array<{ id?: string; name?: string; status?: string; payload?: unknown }>;
};

export type TimelineTurn = {
  turnId?: string;
  role?: 'user' | 'assistant' | string;
  text?: string;
  createdAt?: string;
  cards?: AdvisorCard[];
  agentTrace?: {
    steps?: AgentTraceStep[];
  };
  loading?: boolean;
  failed?: boolean;
  localPending?: boolean;
};

export type AdvisorSessionData = {
  sessionId?: string;
  timeline?: TimelineTurn[];
  workflowStage?: string;
  useWebSearch?: boolean;
  webSearchEnabled?: boolean;
  session?: {
    workflowStage?: string;
    useWebSearch?: boolean;
    webSearchEnabled?: boolean;
  };
  contextBar?: {
    collapsed?: boolean;
    rootPath?: string;
    mode?: { label?: string };
    organizeTaskId?: string;
    directorySummary?: {
      itemCount?: number;
      treeAvailable?: boolean;
    };
    webSearch?: {
      useWebSearch?: boolean;
      webSearchEnabled?: boolean;
      message?: string;
    };
  };
  composer?: {
    placeholder?: string;
    submitLabel?: string;
  };
  [key: string]: unknown;
};

export type StreamHandle = {
  close: () => void;
};

export type PersistFieldKind = 'normal' | 'sensitive' | 'transient';

export type PersistPolicy<T> = {
  key: string;
  kind: PersistFieldKind;
  scopeId?: string;
  defaultValue: T;
  serializer?: (value: T) => string;
  deserializer?: (raw: string) => T;
};
