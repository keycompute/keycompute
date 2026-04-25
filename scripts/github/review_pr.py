import json
import os
import time
from pathlib import Path

import requests


MAX_TOTAL_PATCH_CHARS = int(os.getenv("OPENAI_MAX_TOTAL_PATCH_CHARS", "40_000").replace("_", ""))
MAX_PATCH_CHARS_PER_FILE = int(
    os.getenv("OPENAI_MAX_PATCH_CHARS_PER_FILE", "3_000").replace("_", "")
)
MAX_FILES = int(os.getenv("OPENAI_MAX_FILES", "20"))
MAX_OMITTED_FILES = int(os.getenv("OPENAI_MAX_OMITTED_FILES", "20"))
OPENAI_RETRY_ATTEMPTS = max(int(os.getenv("OPENAI_RETRY_ATTEMPTS", "4")), 1)
OPENAI_INITIAL_BACKOFF_SECONDS = max(
    float(os.getenv("OPENAI_INITIAL_BACKOFF_SECONDS", "5")),
    1.0,
)
OPENAI_MAX_OUTPUT_TOKENS = int(os.getenv("OPENAI_MAX_OUTPUT_TOKENS", "2000"))
MODEL = os.getenv("OPENAI_MODEL", "gpt-5")
API_URL = "https://api.openai.com/v1/responses"
TRANSIENT_STATUS_CODES = {429, 500, 502, 503, 504}
CODE_EXTENSIONS = {
    ".c",
    ".cc",
    ".cpp",
    ".cs",
    ".go",
    ".h",
    ".hpp",
    ".java",
    ".js",
    ".jsx",
    ".kt",
    ".py",
    ".rb",
    ".rs",
    ".swift",
    ".ts",
    ".tsx",
}
CONFIG_EXTENSIONS = {".json", ".toml", ".yaml", ".yml"}
CONTENT_EXTENSIONS = {".css", ".html", ".md", ".scss", ".svg"}
LOW_SIGNAL_FILENAMES = {"Cargo.lock", "package-lock.json", "pnpm-lock.yaml", "yarn.lock"}
TRUNCATION_MARKER = "\n...[truncated]"


class OpenAIRequestError(RuntimeError):
    def __init__(self, message: str, *, status_code: int | None = None, error_type: str = "", error_code: str = ""):
        super().__init__(message)
        self.status_code = status_code
        self.error_type = error_type
        self.error_code = error_code


def load_json(path: str):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def file_score(file_info):
    filename = file_info.get("filename", "")
    path = Path(filename)
    patch = file_info.get("patch") or ""
    churn = file_info.get("additions", 0) + file_info.get("deletions", 0)

    if not patch:
        priority = 0
    elif path.name in LOW_SIGNAL_FILENAMES:
        priority = 1
    elif path.suffix.lower() in CODE_EXTENSIONS:
        priority = 5
    elif path.suffix.lower() in CONFIG_EXTENSIONS:
        priority = 4
    elif path.suffix.lower() in CONTENT_EXTENSIONS:
        priority = 3
    else:
        priority = 2

    return (priority, churn, len(patch))


def summarize_file(file_info):
    return {
        "filename": file_info.get("filename", ""),
        "status": file_info.get("status", ""),
        "additions": file_info.get("additions", 0),
        "deletions": file_info.get("deletions", 0),
    }


def compact_changed_files(files):
    included_files = []
    omitted_files = []
    used_chars = 0
    sorted_files = sorted(files, key=file_score, reverse=True)

    for f in sorted_files:
        patch = f.get("patch") or ""
        if not patch:
            omitted_files.append(summarize_file(f))
            continue

        if len(included_files) >= MAX_FILES or used_chars >= MAX_TOTAL_PATCH_CHARS:
            omitted_files.append(summarize_file(f))
            continue

        remaining = MAX_TOTAL_PATCH_CHARS - used_chars
        if remaining <= 0:
            omitted_files.append(summarize_file(f))
            continue

        patch_budget = min(remaining, MAX_PATCH_CHARS_PER_FILE)
        patch_truncated = len(patch) > patch_budget
        if patch_truncated:
            if patch_budget <= len(TRUNCATION_MARKER):
                patch = TRUNCATION_MARKER[:patch_budget]
            else:
                patch = patch[: patch_budget - len(TRUNCATION_MARKER)] + TRUNCATION_MARKER

        used_chars += len(patch)

        included_files.append(
            {
                "filename": f.get("filename", ""),
                "status": f.get("status", ""),
                "additions": f.get("additions", 0),
                "deletions": f.get("deletions", 0),
                "patch": patch,
                "patch_truncated": patch_truncated,
            }
        )

    return {
        "included_files": included_files,
        "included_count": len(included_files),
        "omitted_count": len(omitted_files),
        "omitted_files": omitted_files[:MAX_OMITTED_FILES],
        "total_patch_chars": used_chars,
    }


def build_instructions():
    return """You are a senior code reviewer with strong experience in Rust monorepos, multi-crate workspaces, backend gateway systems, authentication, billing, routing, and API integrations.

Review rules:
1. Only report issues supported by evidence in the diff.
2. Do not speculate about unseen code. If something is uncertain, label it as a hypothesis.
3. Prioritize:
   - correctness
   - security
   - error handling
   - edge cases
   - maintainability
   - missing tests
4. Distinguish clearly between blocking issues and non-blocking suggestions.
5. If no obvious blocking issue is found, say so clearly.
6. Keep the review concise, concrete, and diff-focused.
7. Reference filenames whenever possible.

Output must be English Markdown with this exact structure:

## Title
## Overall Assessment
## Blocking Issues
## Non-blocking Suggestions
## Suggested Tests
## Conclusion
"""


def build_input(repo_name, pr, compact_files, comment_body, comment_author):
    payload = {
        "repository": repo_name,
        "pr_number": pr.get("number"),
        "title": pr.get("title", ""),
        "author": pr.get("user", {}).get("login", ""),
        "base_branch": pr.get("base", {}).get("ref", ""),
        "head_branch": pr.get("head", {}).get("ref", ""),
        "changed_files_count": pr.get("changed_files", 0),
        "review_scope": {
            "included_files": compact_files["included_count"],
            "omitted_files": compact_files["omitted_count"],
            "total_patch_chars": compact_files["total_patch_chars"],
        },
        "trigger_comment_author": comment_author,
        "trigger_comment": comment_body,
        "pr_description": pr.get("body", ""),
        "changed_files": compact_files["included_files"],
        "omitted_files": compact_files["omitted_files"],
    }

    return (
        "Review the following GitHub pull request diff.\n\n"
        "Return a practical PR review for maintainers.\n\n"
        f"{json.dumps(payload, ensure_ascii=False, indent=2)}"
    )


def extract_error_message(response: requests.Response) -> str:
    try:
        data = response.json()
    except ValueError:
        return response.text.strip()

    error = data.get("error")
    if isinstance(error, dict):
        message = error.get("message")
        if message:
            return message
        return json.dumps(error, ensure_ascii=False)

    if isinstance(error, str):
        return error

    return response.text.strip()


def extract_error_details(response: requests.Response) -> tuple[str, str, str]:
    try:
        data = response.json()
    except ValueError:
        return "", "", response.text.strip()

    error = data.get("error")
    if not isinstance(error, dict):
        if isinstance(error, str):
            return "", "", error
        return "", "", response.text.strip()

    error_type = str(error.get("type") or "").strip()
    error_code = str(error.get("code") or "").strip()
    error_message = str(error.get("message") or "").strip()
    if not error_message:
        error_message = json.dumps(error, ensure_ascii=False)
    return error_type, error_code, error_message


def retry_delay_seconds(response: requests.Response, attempt: int) -> float:
    retry_after = response.headers.get("retry-after")
    if retry_after:
        try:
            return max(float(retry_after), 1.0)
        except ValueError:
            pass

    return OPENAI_INITIAL_BACKOFF_SECONDS * (2**attempt)


def is_insufficient_quota(status_code: int, error_type: str, error_code: str, error_message: str) -> bool:
    lowered = f"{error_type} {error_code} {error_message}".lower()
    return status_code == 429 and (
        "insufficient_quota" in lowered
        or "billing_hard_limit_reached" in lowered
        or "exceeded your current quota" in lowered
    )


def call_openai(instructions: str, user_input: str) -> str:
    api_key = os.environ.get("OPENAI_API_KEY")
    if not api_key:
        raise RuntimeError("OPENAI_API_KEY is not set")

    data = None
    for attempt in range(OPENAI_RETRY_ATTEMPTS):
        response = requests.post(
            API_URL,
            headers={
                "Authorization": f"Bearer {api_key}",
                "Content-Type": "application/json",
            },
            json={
                "model": MODEL,
                "instructions": instructions,
                "input": user_input,
                "max_output_tokens": OPENAI_MAX_OUTPUT_TOKENS,
            },
            timeout=180,
        )

        if response.ok:
            data = response.json()
            break

        error_type, error_code, error_message = extract_error_details(response)
        should_retry = (
            response.status_code in TRANSIENT_STATUS_CODES
            and not is_insufficient_quota(response.status_code, error_type, error_code, error_message)
            and attempt < OPENAI_RETRY_ATTEMPTS - 1
        )
        if should_retry:
            time.sleep(retry_delay_seconds(response, attempt))
            continue

        if is_insufficient_quota(response.status_code, error_type, error_code, error_message):
            raise OpenAIRequestError(
                "OpenAI API quota exceeded (429). "
                f"Model: {MODEL}. "
                f"Type: {error_type or 'unknown'}. "
                f"Code: {error_code or 'unknown'}. "
                f"Details: {error_message or 'Too Many Requests'}",
                status_code=response.status_code,
                error_type=error_type,
                error_code=error_code,
            )

        if response.status_code == 429:
            raise OpenAIRequestError(
                "OpenAI API rate limit exceeded (429). "
                f"Model: {MODEL}. "
                f"Type: {error_type or 'unknown'}. "
                f"Code: {error_code or 'unknown'}. "
                f"Details: {error_message or 'Too Many Requests'}",
                status_code=response.status_code,
                error_type=error_type,
                error_code=error_code,
            )

        response.raise_for_status()

    if data is None:
        raise RuntimeError("OpenAI API request did not return a usable response")

    if data.get("error"):
        raise RuntimeError(f"OpenAI API error: {data['error']}")

    output_text = data.get("output_text")
    if output_text:
        return output_text.strip()

    parts = []
    for item in data.get("output", []):
        for content in item.get("content", []):
            text = content.get("text")
            if isinstance(text, str) and text.strip():
                parts.append(text)

    return "\n".join(parts).strip()


def write_output(review: str):
    if not review:
        review = """## Title
GPT PR Review

## Overall Assessment
No usable review output was returned.

## Blocking Issues
- Unable to parse model output.

## Non-blocking Suggestions
- Check the OpenAI API response format and workflow logs.

## Suggested Tests
- Re-run the workflow on the same PR.
- Verify that pr.json and pr_files.json were generated correctly.

## Conclusion
The review did not complete successfully.
"""

    footer = """

---

Trigger: `@gpt review`  
Note: This review is generated from the PR diff and may not reflect code outside the visible changes.
"""
    Path("review-output.md").write_text(review + footer, encoding="utf-8")


def build_failure_review(error: Exception) -> str:
    error_text = f"{type(error).__name__}: {str(error)}"
    suggestions = [
        "- Check repository secrets.",
        "- Check OpenAI API access and model availability.",
        "- Check the workflow logs for the request/response path.",
    ]
    tests = [
        "- Re-run the workflow.",
        "- Verify `OPENAI_API_KEY`.",
        "- Verify that `pr.json` and `pr_files.json` contain valid data.",
    ]

    lowered = error_text.lower()
    if "quota exceeded" in lowered or "insufficient_quota" in lowered or "billing_hard_limit_reached" in lowered:
        suggestions = [
            "- The request reached the OpenAI project quota limit; this is usually not caused by a missing repository secret.",
            "- Add quota/billing to the project behind `OPENAI_API_KEY`, or switch to a project with available spend.",
            "- Keep the configured model unchanged and retry after the project quota has been restored.",
        ]
        tests = [
            "- Call the same model from the same API project outside GitHub Actions to confirm the quota error is reproducible.",
            "- Verify the API project attached to `OPENAI_API_KEY` has active billing and remaining quota.",
            "- Re-run the workflow after quota is restored.",
        ]
    elif "429" in error_text or "rate limit" in lowered:
        suggestions = [
            "- The request hit an OpenAI rate limit; the key may still be valid.",
            "- Retry after a short wait, or reduce the diff context and retry with the same comment trigger.",
            "- If this happens often, reduce prompt size or lower concurrent workflow runs for this repository.",
        ]
        tests = [
            "- Re-run the workflow after a short delay.",
            "- Trigger the review on a smaller PR or after trimming the prompt budget.",
            "- Confirm the API project attached to `OPENAI_API_KEY` can still call the configured model.",
        ]

    return f"""## Title
GPT PR Review

## Overall Assessment
The automated review failed before producing a normal result.

## Blocking Issues
- Workflow/runtime error: `{error_text}`

## Non-blocking Suggestions
{chr(10).join(suggestions)}

## Suggested Tests
{chr(10).join(tests)}

## Conclusion
The review could not be completed due to an execution error.
"""


def main():
    repo_name = os.environ.get("REPO_NAME", "")
    comment_body = os.environ.get("COMMENT_BODY", "")
    comment_author = os.environ.get("COMMENT_AUTHOR", "")

    pr = load_json("pr.json")
    pr_files = load_json("pr_files.json")

    compact_files = compact_changed_files(pr_files)
    instructions = build_instructions()
    user_input = build_input(
        repo_name=repo_name,
        pr=pr,
        compact_files=compact_files,
        comment_body=comment_body,
        comment_author=comment_author,
    )

    try:
        review = call_openai(instructions, user_input)
    except Exception as e:
        review = build_failure_review(e)

    write_output(review)


if __name__ == "__main__":
    main()
