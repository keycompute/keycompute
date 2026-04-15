#!/usr/bin/env python3
"""
简单的对话 Demo，使用 OpenAI 兼容格式的 API（流式响应）
"""

import json
import requests

# 配置

# API_URL = "https://api.deepseek.com/v1/chat/completions"
# API_KEY = "sk-70f08cda30ee4e56bd0d27223dec522f"
# API_MODEL = "deepseek-chat"

API_URL = "http://192.168.100.100:3000/v1/chat/completions"
API_KEY = "sk-5f65b519de874eb9a9a2a97f1a12d1c972e6bed188664d24"
API_MODEL = "deepseek-chat"


def chat():
    messages = []
    print("对话已开始，输入 'quit' 退出\n")

    while True:
        user_input = input("你: ").strip()
        if user_input.lower() == "quit":
            break
        if not user_input:
            continue

        messages.append({"role": "user", "content": user_input})

        try:
            headers = {
                "Content-Type": "application/json",
                "Authorization": f"Bearer {API_KEY}",
            }
            data = {
                "model": API_MODEL,
                "messages": messages,
                "stream": True,
            }

            print("助手: ", end="", flush=True)
            reply_parts = []

            # 使用 requests 发送流式请求
            with requests.post(API_URL, headers=headers, json=data, stream=True) as response:
                response.raise_for_status()

                # 逐行读取流式响应
                for line in response.iter_lines():
                    if not line:
                        continue

                    line = line.decode("utf-8")

                    # SSE 格式以 "data: " 开头
                    if line.startswith("data: "):
                        json_str = line[6:]  # 去掉 "data: " 前缀

                        # 流结束标记
                        if json_str.strip() == "[DONE]":
                            break

                        try:
                            chunk = json.loads(json_str)
                            delta = chunk["choices"][0].get("delta", {})
                            content = delta.get("content", "")
                            if content:
                                print(content, end="", flush=True)
                                reply_parts.append(content)
                        except (json.JSONDecodeError, KeyError, IndexError):
                            continue

            print()  # 换行
            print()

            # 合并所有内容
            reply = "".join(reply_parts)
            if reply:
                messages.append({"role": "assistant", "content": reply})

        except Exception as e:
            print(f"\n错误: {e}\n")
            # 移除失败的消息，允许重试
            messages.pop()


if __name__ == "__main__":
    chat()
