export type PrivacyMode = "send_raw" | "send_hashed" | "do_not_send";
export type UnpauseAction =
  | "if_already_ready"
  | "if_others_ready"
  | "if_min_users_ready"
  | "always";
export type ChatInputPosition = "top" | "middle" | "bottom";
export type ChatOutputMode = "chatroom" | "scrolling";
export type TransparencyMode = "off" | "low" | "high";

export interface ServerConfig {
  host: string;
  port: number;
  password: string | null;
}

export interface PublicServer {
  name: string;
  address: string;
}

export interface UserPreferences {
  username: string;
  default_room: string;
  room_list: string[];
  theme: string;
  transparency_mode: TransparencyMode;

  seek_threshold_rewind: number;
  seek_threshold_fastforward: number;
  slowdown_threshold: number;
  slowdown_reset_threshold: number;
  slowdown_rate: number;
  slow_on_desync: boolean;
  rewind_on_desync: boolean;
  fastforward_on_desync: boolean;
  dont_slow_down_with_me: boolean;

  ready_at_start: boolean;
  pause_on_leave: boolean;
  unpause_action: UnpauseAction;
  autoplay_enabled: boolean;
  autoplay_min_users: number;
  autoplay_require_same_filenames: boolean;

  filename_privacy_mode: PrivacyMode;
  filesize_privacy_mode: PrivacyMode;

  only_switch_to_trusted_domains: boolean;
  trusted_domains: string[];

  show_osd: boolean;
  osd_duration: number;
  show_osd_warnings: boolean;
  show_slowdown_osd: boolean;
  show_different_room_osd: boolean;
  show_same_room_osd: boolean;
  show_non_controller_osd: boolean;
  show_duration_notification: boolean;

  chat_input_enabled: boolean;
  chat_direct_input: boolean;
  chat_input_font_family: string;
  chat_input_relative_font_size: number;
  chat_input_font_weight: number;
  chat_input_font_underline: boolean;
  chat_input_font_color: string;
  chat_input_position: ChatInputPosition;
  chat_output_enabled: boolean;
  chat_output_font_family: string;
  chat_output_relative_font_size: number;
  chat_output_font_weight: number;
  chat_output_font_underline: boolean;
  chat_output_mode: ChatOutputMode;
  chat_max_lines: number;
  chat_top_margin: number;
  chat_left_margin: number;
  chat_bottom_margin: number;
  chat_move_osd: boolean;
  chat_osd_margin: number;
  notification_timeout: number;
  alert_timeout: number;
  chat_timeout: number;

  autosave_joins_to_list: boolean;
  shared_playlist_enabled: boolean;
  loop_at_end_of_playlist: boolean;
  loop_single_files: boolean;
  show_playlist: boolean;
  side_panel_layout: "rows" | "columns";
  auto_connect: boolean;
  force_gui_prompt: boolean;
  check_for_updates_automatically: boolean | null;
  debug: boolean;
}

export interface PlayerConfig {
  player_path: string;
  media_directories: string[];
  player_arguments: string[];
  per_player_arguments: Record<string, string[]>;
}

export interface SyncplayConfig {
  server: ServerConfig;
  user: UserPreferences;
  player: PlayerConfig;
  recent_servers: ServerConfig[];
  public_servers: PublicServer[];
}
