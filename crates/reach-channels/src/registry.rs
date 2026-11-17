//! The frozen channel registry: 14 channels, 40 actions, Agent-Reach 1.6.1.
//!
//! Every field here is data recorded from the Python build
//! (`tests/golden/registry.json`); `tests/registry_parity.rs` fails if a name,
//! help string, param, default, backend pair, timeout or public-web scope drifts.

use serde_json::{json, Value};
use std::sync::LazyLock;

/// One argument of a unified action.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub help: String,
    pub required: bool,
    /// Python type name upstream prints in `channels` (`str` / `int`).
    pub kind: String,
    pub default: Value,
}

/// Login surface of a channel, as `channels` reports it.
#[derive(Debug, Clone)]
pub struct Login {
    /// `browser` (dedicated browser login) or `session` (upstream CLI session).
    pub via: String,
    pub command: String,
}

/// A unified action (`<channel> <action>`).
#[derive(Debug, Clone)]
pub struct Action {
    pub name: String,
    pub help: String,
    pub params: Vec<Param>,
    /// Ordered `(backend label, upstream method name)` pairs.
    pub backends: Vec<(String, String)>,
    /// Default timeout for slow actions; `None` = global default.
    pub timeout: Option<f64>,
    /// Public URL scopes tried through the web channel before site tools.
    pub public_web: Vec<String>,
    /// Upstream `live_test` inputs, used by `check`.
    pub live_test: Option<Value>,
}

impl Action {
    /// Whether this action declares `label` as one of its backends.
    pub fn has_backend(&self, label: &str) -> bool {
        self.backends
            .iter()
            .any(|(candidate, _)| candidate == label)
    }
}

/// A platform facade: identity, login surface, and its actions.
#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub description: String,
    /// `default` / `optional` / `experimental`.
    pub level: String,
    pub actions: Vec<Action>,
    /// Upstream CLI whose login lives in the session directory (`""` = none).
    pub session: String,
    /// Page the user logs in on in the dedicated browser (`""` = none).
    pub login_url: String,
    /// Hosts this channel claims for `read` routing.
    pub domains: Vec<String>,
    /// Upstream tools `add` installs.
    pub tools: Vec<String>,
    /// Machine prerequisites `add` installs when missing.
    pub prereqs: Vec<String>,
    /// Config key the user must provide before the channel works (`""` = none).
    pub needs_key: String,
    /// Cookie only a logged-in user has; its absence explains a failed read.
    pub login_cookie: String,
    /// Run tool calls with `HOME` = the session directory.
    pub session_home: bool,
}

/// Required string parameter.
fn p(name: &str, help: &str) -> Param {
    Param {
        name: name.into(),
        help: help.into(),
        required: true,
        kind: "str".into(),
        default: Value::Null,
    }
}

/// Optional integer parameter with a default.
fn pi(name: &str, help: &str, default: i64) -> Param {
    Param {
        name: name.into(),
        help: help.into(),
        required: false,
        kind: "int".into(),
        default: json!(default),
    }
}

/// Optional string parameter without a default.
fn ps(name: &str, help: &str) -> Param {
    Param {
        name: name.into(),
        help: help.into(),
        required: false,
        kind: "str".into(),
        default: Value::Null,
    }
}

/// Optional string parameter with a default.
fn psd(name: &str, help: &str, default: &str) -> Param {
    Param {
        name: name.into(),
        help: help.into(),
        required: false,
        kind: "str".into(),
        default: json!(default),
    }
}

fn action(name: &str, help: &str, params: Vec<Param>, backends: &[(&str, &str)]) -> Action {
    Action {
        name: name.into(),
        help: help.into(),
        params,
        backends: backends
            .iter()
            .map(|(label, method)| ((*label).to_string(), (*method).to_string()))
            .collect(),
        timeout: None,
        public_web: Vec::new(),
        live_test: None,
    }
}

impl Action {
    fn timeout(mut self, seconds: f64) -> Self {
        self.timeout = Some(seconds);
        self
    }

    fn public_web(mut self, scopes: &[&str]) -> Self {
        self.public_web = scopes.iter().map(|scope| (*scope).to_string()).collect();
        self
    }

    fn live_test(mut self, inputs: Value) -> Self {
        self.live_test = Some(inputs);
        self
    }
}

fn platform(name: &str, description: &str, level: &str, actions: Vec<Action>) -> Channel {
    Channel {
        name: name.into(),
        description: description.into(),
        level: level.into(),
        actions,
        session: String::new(),
        login_url: String::new(),
        domains: Vec::new(),
        tools: Vec::new(),
        prereqs: Vec::new(),
        needs_key: String::new(),
        login_cookie: String::new(),
        session_home: true,
    }
}

impl Channel {
    fn domains(mut self, hosts: &[&str]) -> Self {
        self.domains = hosts.iter().map(|host| (*host).to_string()).collect();
        self
    }

    fn login_url(mut self, url: &str) -> Self {
        self.login_url = url.into();
        self
    }

    fn session(mut self, tool: &str, home: bool) -> Self {
        self.session = tool.into();
        self.session_home = home;
        self
    }

    fn tools(mut self, tools: &[&str]) -> Self {
        self.tools = tools.iter().map(|tool| (*tool).to_string()).collect();
        self
    }

    fn prereqs(mut self, prereqs: &[&str]) -> Self {
        self.prereqs = prereqs.iter().map(|item| (*item).to_string()).collect();
        self
    }

    fn needs_key(mut self, key: &str) -> Self {
        self.needs_key = key.into();
        self
    }

    fn login_cookie(mut self, cookie: &str) -> Self {
        self.login_cookie = cookie.into();
        self
    }

    /// Login surface as `channels` reports it.
    pub fn login(&self) -> Option<Login> {
        if !self.login_url.is_empty() {
            Some(Login {
                via: "browser".into(),
                command: format!("arana login {}", self.name),
            })
        } else if !self.session.is_empty() {
            Some(Login {
                via: "session".into(),
                command: format!("arana login {}", self.name),
            })
        } else {
            None
        }
    }
}

fn github_channel() -> Channel {
    platform(
        "github",
        "GitHub 仓库和代码",
        "default",
        vec![
            action(
                "search",
                "Search repositories",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("gh", "_gh_search"), ("github-api", "_api_search")],
            )
            .live_test(json!({"query": "yt-dlp", "limit": 2})),
            action(
                "read",
                "Read a repository (with README) or an issue / PR (with comments)",
                vec![p("target", "owner/repo, or a repo / issue / PR link")],
                &[("gh", "_gh_read"), ("github-api", "_api_read")],
            )
            .live_test(json!({"target": "yt-dlp/yt-dlp"})),
        ],
    )
    .domains(&["github.com"])
}

fn twitter_channel() -> Channel {
    platform(
        "twitter",
        "Twitter/X 推文",
        "experimental",
        vec![
            action(
                "search",
                "Search tweets",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("opencli", "_opencli_search")],
            )
            .public_web(&["x.com/", "twitter.com/"])
            .live_test(json!({"query": "openai", "limit": 2})),
            action(
                "profile",
                "A user's profile",
                vec![p("username", "handle without @")],
                &[("opencli", "_opencli_profile")],
            )
            .live_test(json!({"username": "OpenAI"})),
            action(
                "tweets",
                "A user's recent tweets",
                vec![
                    p("username", "handle without @"),
                    pi("limit", "max tweets", 5),
                ],
                &[("opencli", "_opencli_tweets")],
            ),
            action(
                "thread",
                "A tweet with its replies",
                vec![p("target", "tweet id or link")],
                &[("opencli", "_opencli_thread")],
            ),
        ],
    )
    .domains(&["x.com", "twitter.com"])
    .login_url("https://x.com/i/flow/login")
    .tools(&["opencli"])
    .prereqs(&["node", "chrome"])
}

fn youtube_channel() -> Channel {
    platform(
        "youtube",
        "YouTube 视频和字幕",
        "default",
        vec![
            action(
                "read",
                "Video title, channel, date and description",
                vec![p("target", "video link or id")],
                &[("yt-dlp", "_ytdlp_read")],
            )
            .live_test(json!({"target": "https://www.youtube.com/watch?v=jNQXAC9IVRw"})),
            action(
                "search",
                "Search videos",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("yt-dlp", "_ytdlp_search")],
            )
            .live_test(json!({"query": "python tutorial", "limit": 2})),
            action(
                "subtitle",
                "Full subtitle text of a video (manual or auto captions)",
                vec![
                    p("target", "video link or id"),
                    psd(
                        "lang",
                        "preferred languages, comma separated",
                        "zh-Hans,zh,en,en-orig",
                    ),
                ],
                &[("yt-dlp", "_ytdlp_subtitle")],
            )
            .live_test(json!({"target": "https://www.youtube.com/watch?v=jNQXAC9IVRw"})),
            action(
                "transcribe",
                "Speech-to-text of a video's audio (uses Groq/OpenAI Whisper quota)",
                vec![p("target", "video link or id")],
                &[("whisper", "_whisper_transcribe")],
            )
            .timeout(1800.0),
        ],
    )
    .domains(&["youtube.com", "youtu.be"])
    .prereqs(&["ffmpeg"])
}

fn reddit_channel() -> Channel {
    platform(
        "reddit",
        "Reddit 帖子和评论",
        "optional",
        vec![
            action(
                "search",
                "Search public posts first; --backend opencli uses site search",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("opencli", "_opencli_search")],
            )
            .public_web(&["reddit.com/r/"])
            .live_test(json!({"query": "python", "limit": 2})),
            action(
                "read",
                "Read public page text first; --backend opencli requests the post and top comments",
                vec![p("target", "post link or id")],
                &[("opencli", "_opencli_read")],
            )
            .public_web(&["reddit.com/r/", "redd.it/"]),
            action(
                "subreddit",
                "Latest posts in a subreddit",
                vec![
                    p("name", "subreddit name without r/, e.g. python"),
                    pi("limit", "max posts", 5),
                ],
                &[("opencli", "_opencli_subreddit")],
            )
            .live_test(json!({"name": "python", "limit": 2})),
        ],
    )
    .domains(&["reddit.com", "redd.it"])
    .login_url("https://www.reddit.com/login/")
    .tools(&["opencli"])
    .prereqs(&["node", "chrome"])
}

fn facebook_channel() -> Channel {
    platform(
        "facebook",
        "Facebook 帖子、主页和群组",
        "experimental",
        vec![
            action(
                "search",
                "Search Facebook",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("opencli", "_opencli_search")],
            )
            .public_web(&["facebook.com/"])
            .live_test(json!({"query": "openai", "limit": 2})),
            action(
                "profile",
                "A user's or page's profile",
                vec![p("username", "username or page name")],
                &[("opencli", "_opencli_profile")],
            )
            .live_test(json!({"username": "zuck"})),
        ],
    )
    .domains(&["facebook.com", "fb.com", "fb.watch"])
    .login_url("https://www.facebook.com/login")
    .tools(&["opencli"])
    .prereqs(&["node", "chrome"])
}

fn instagram_channel() -> Channel {
    platform(
        "instagram",
        "Instagram 用户、主页和指定用户帖子",
        "experimental",
        vec![
            action(
                "search",
                "Search users (not posts)",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("opencli", "_opencli_search")],
            )
            .live_test(json!({"query": "nasa", "limit": 2})),
            action(
                "profile",
                "A user's profile",
                vec![p("username", "Instagram username")],
                &[("opencli", "_opencli_profile")],
            )
            .live_test(json!({"username": "nasa"})),
            action(
                "posts",
                "A user's recent posts",
                vec![
                    p("username", "Instagram username"),
                    pi("limit", "max posts", 6),
                ],
                &[("opencli", "_opencli_posts")],
            )
            .live_test(json!({"username": "nasa", "limit": 2})),
        ],
    )
    .domains(&["instagram.com", "instagr.am"])
    .login_url("https://www.instagram.com/accounts/login/")
    .tools(&["opencli"])
    .prereqs(&["node", "chrome"])
    .login_cookie("sessionid")
}

fn bilibili_channel() -> Channel {
    platform(
        "bilibili",
        "B站视频、字幕和搜索",
        "default",
        vec![
            action(
                "search",
                "Search videos",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("bili-cli", "_cli_search")],
            )
            .live_test(json!({"query": "Python", "limit": 2})),
            action(
                "read",
                "Video title, uploader, description and stats",
                vec![p("target", "BV id or video link")],
                &[("bili-cli", "_cli_read"), ("bilibili-api", "_api_read")],
            )
            .live_test(json!({"target": "BV1GJ411x7h7"})),
            action(
                "hot",
                "Trending videos",
                vec![pi("limit", "max items", 10)],
                &[("bili-cli", "_cli_hot"), ("bilibili-api", "_api_hot")],
            )
            .live_test(json!({"limit": 2})),
            action(
                "subtitle",
                "Subtitle text of the first video part, when available (may need a Bilibili login)",
                vec![p("target", "BV id or video link")],
                &[("bili-cli", "_cli_subtitle")],
            )
            .live_test(json!({"target": "BV1GJ411x7h7"})),
        ],
    )
    .domains(&["bilibili.com", "b23.tv"])
    .session("bili-cli", true)
    .tools(&["bili-cli"])
}

fn xiaohongshu_channel() -> Channel {
    platform(
        "xiaohongshu",
        "小红书笔记",
        "optional",
        vec![
            action(
                "search",
                "Search notes",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("opencli", "_opencli_search")],
            )
            .public_web(&[
                "xiaohongshu.com/explore/",
                "xiaohongshu.com/search_result/",
                "xhslink.com/",
            ])
            .live_test(json!({"query": "咖啡", "limit": 2})),
            action(
                "read",
                "Read a note (full link including xsec_token, as returned by search)",
                vec![p("target", "full note link with xsec_token")],
                &[("opencli", "_opencli_read")],
            )
            .public_web(&[
                "xiaohongshu.com/explore/",
                "xiaohongshu.com/search_result/",
                "xhslink.com/",
            ]),
            action(
                "comments",
                "Read loaded comments and replies from a note; not an exhaustive export",
                vec![
                    p("target", "full note link with xsec_token"),
                    pi(
                        "limit",
                        "max top-level comments (1..50); loaded replies are included",
                        20,
                    ),
                ],
                &[("opencli", "_opencli_comments")],
            )
            .timeout(60.0),
        ],
    )
    .domains(&["xiaohongshu.com", "xhslink.com"])
    .login_url("https://www.xiaohongshu.com/explore")
    .tools(&["opencli"])
    .prereqs(&["node", "chrome"])
}

fn linkedin_channel() -> Channel {
    platform(
        "linkedin",
        "LinkedIn 职位搜索与详情",
        "optional",
        vec![
            action(
                "jobs",
                "Search LinkedIn jobs",
                vec![
                    p("query", "job keywords"),
                    ps("location", "city or country"),
                    pi("limit", "max results", 5),
                ],
                &[("opencli", "_jobs")],
            )
            .timeout(120.0)
            .public_web(&["linkedin.com/jobs/view/"])
            .live_test(json!({"query": "python", "limit": 2})),
            action(
                "read",
                "Read a LinkedIn job description",
                vec![p("target", "full job URL")],
                &[("opencli", "_read")],
            )
            .timeout(90.0)
            .public_web(&["linkedin.com/jobs/view/"]),
        ],
    )
    .domains(&["linkedin.com"])
    .login_url("https://www.linkedin.com/login")
    .tools(&["opencli"])
    .prereqs(&["node", "chrome"])
    .login_cookie("li_at")
}

fn boss_channel() -> Channel {
    platform(
        "boss",
        "Boss直聘 职位搜索与 JD",
        "optional",
        vec![
            action(
                "search",
                "Search jobs using a configured browser session",
                vec![
                    p("query", "job keywords"),
                    ps("city", "city, e.g. 深圳"),
                    pi("limit", "max results", 10),
                ],
                &[
                    ("opencli", "_opencli_search"),
                    ("boss-agent-cli", "_cli_search"),
                ],
            )
            .timeout(90.0)
            .public_web(&["zhipin.com/job_detail/"])
            .live_test(json!({"query": "Python", "city": "深圳", "limit": 3})),
            action(
                "read",
                "Read a direct BOSS job URL",
                vec![p("target", "full job URL")],
                &[
                    ("boss-agent-cli", "_cli_read"),
                    ("opencli", "_opencli_read"),
                ],
            )
            .timeout(90.0)
            .public_web(&["zhipin.com/job_detail/"]),
        ],
    )
    .domains(&["zhipin.com"])
    .login_url("https://www.zhipin.com/web/user/?ka=header-login")
    .session("boss-agent-cli", false)
    .tools(&["boss-agent-cli"])
    .prereqs(&["chrome"])
}

fn v2ex_channel() -> Channel {
    platform(
        "v2ex",
        "V2EX 节点、主题与回复",
        "default",
        vec![
            action(
                "hot",
                "Today's hot topics",
                vec![pi("limit", "max items", 10)],
                &[("v2ex-api", "_api_hot")],
            )
            .live_test(json!({"limit": 3})),
            action(
                "node",
                "Latest topics in a node",
                vec![
                    p("node_name", "node name, e.g. python, programmer, jobs"),
                    pi("limit", "max items", 10),
                ],
                &[("v2ex-api", "_api_node")],
            )
            .live_test(json!({"node_name": "python", "limit": 3})),
            action(
                "read",
                "Read a topic and its replies",
                vec![p("target", "topic link or id")],
                &[("v2ex-api", "_api_read")],
            )
            .live_test(json!({"target": "https://www.v2ex.com/t/1000"})),
            action(
                "user",
                "A member's profile",
                vec![p("username", "V2EX username")],
                &[("v2ex-api", "_api_user")],
            )
            .live_test(json!({"username": "Livid"})),
        ],
    )
    .domains(&["v2ex.com"])
}

fn xueqiu_channel() -> Channel {
    platform(
        "xueqiu",
        "雪球股票行情与社区动态",
        "optional",
        vec![
            action(
                "quote",
                "Real-time stock quote (A-share, HK, US)",
                vec![p("symbol", "e.g. SH600519, 600519, 00700, AAPL")],
                &[("tencent", "_tencent_quote"), ("xueqiu-api", "_xq_quote")],
            )
            .live_test(json!({"symbol": "SH600519"})),
            action(
                "search",
                "Find stocks by name or code",
                vec![
                    p("query", "name or code, e.g. 茅台"),
                    pi("limit", "max results", 5),
                ],
                &[("tencent", "_tencent_search"), ("xueqiu-api", "_xq_search")],
            )
            .live_test(json!({"query": "茅台", "limit": 3})),
            action(
                "hot_posts",
                "Trending Xueqiu community posts (needs a browser session)",
                vec![pi("limit", "max items", 10)],
                &[("xueqiu-api", "_xq_hot_posts")],
            )
            .live_test(json!({"limit": 3})),
            action(
                "hot_stocks",
                "Most-watched stocks on Xueqiu (needs a browser session)",
                vec![pi("limit", "max items", 10)],
                &[("xueqiu-api", "_xq_hot_stocks")],
            )
            .live_test(json!({"limit": 3})),
        ],
    )
    .domains(&["xueqiu.com"])
    .login_url("https://xueqiu.com/")
}

fn xiaoyuzhou_channel() -> Channel {
    platform(
        "xiaoyuzhou",
        "小宇宙播客转文字",
        "optional",
        vec![action(
            "transcribe",
            "Full transcript of a podcast episode (uses Groq Whisper quota)",
            vec![p("target", "episode link")],
            &[("groq-whisper", "_script_transcribe")],
        )
        .timeout(900.0)],
    )
    .domains(&["xiaoyuzhoufm.com"])
    .prereqs(&["ffmpeg"])
    .needs_key("groq-key")
}

fn web_channel() -> Channel {
    platform(
        "web",
        "网页搜索与阅读（Agent 自带工具不可用时的保底）",
        "default",
        vec![
            action(
                "search",
                "Search the web (Exa semantic search)",
                vec![
                    p("query", "what to search for"),
                    pi("limit", "max results", 5),
                ],
                &[("exa", "_exa_search")],
            )
            .live_test(json!({"query": "yt-dlp github repository", "limit": 2})),
            action(
                "read",
                "Read a web page as clean text",
                vec![p("target", "page URL")],
                &[("jina", "_jina_read"), ("exa", "_exa_read")],
            )
            .live_test(json!({"target": "https://example.com"})),
        ],
    )
}

/// The registry, in upstream order.
static CHANNELS: LazyLock<Vec<Channel>> = LazyLock::new(|| {
    vec![
        github_channel(),
        twitter_channel(),
        youtube_channel(),
        reddit_channel(),
        facebook_channel(),
        instagram_channel(),
        bilibili_channel(),
        xiaohongshu_channel(),
        linkedin_channel(),
        boss_channel(),
        v2ex_channel(),
        xueqiu_channel(),
        xiaoyuzhou_channel(),
        web_channel(),
    ]
});

/// Every channel, in upstream registry order.
pub fn registry() -> &'static Vec<Channel> {
    &CHANNELS
}

/// Look up a channel by name.
pub fn channel(name: &str) -> Option<&'static Channel> {
    CHANNELS.iter().find(|channel| channel.name == name)
}

/// The channel `read <url>` must use: the first non-web channel in registry
/// order that exposes `read` and matches the URL, else the web fallback.
pub fn route(url: &str) -> &'static Channel {
    CHANNELS
        .iter()
        .find(|channel| {
            channel.name != "web" && channel.action("read").is_some() && channel.can_handle(url)
        })
        .or_else(|| CHANNELS.iter().find(|channel| channel.name == "web"))
        .expect("the web channel is always registered")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_the_frozen_catalog() {
        assert_eq!(registry().len(), 14);
        let total: usize = registry().iter().map(|c| c.actions.len()).sum();
        assert_eq!(total, 40);
    }

    #[test]
    fn routing_prefers_specific_channels_and_falls_back_to_web() {
        assert_eq!(route("https://www.v2ex.com/t/1000").name, "v2ex");
        assert_eq!(route("https://github.com/yt-dlp/yt-dlp").name, "github");
        assert_eq!(
            route("https://www.bilibili.com/video/BV1GJ411x7h7").name,
            "bilibili"
        );
        // twitter exposes `thread`, not `read`, so an x.com link falls to web.
        assert_eq!(route("https://x.com/OpenAI/status/1").name, "web");
        assert_eq!(
            route("https://www.reddit.com/r/python/comments/1/x").name,
            "reddit"
        );
        assert_eq!(route("https://example.com/page").name, "web");
        // youtube has `read`; xiaoyuzhou does not, so its URLs fall to web.
        assert_eq!(route("https://www.youtube.com/watch?v=x").name, "youtube");
        assert_eq!(route("https://www.xiaoyuzhoufm.com/episode/1").name, "web");
    }

    #[test]
    fn login_surfaces_match_upstream() {
        assert_eq!(channel("bilibili").unwrap().login().unwrap().via, "session");
        assert_eq!(channel("xueqiu").unwrap().login().unwrap().via, "browser");
        assert!(channel("v2ex").unwrap().login().is_none());
    }
}
