// kasa-op — 1Password 서비스 계정 토큰으로 비밀을 읽는 kasaterm 전용 실행기(docs/op-faceid-approval.md).
//
// 토큰은 표준 입력으로만 받는다. op CLI 는 토큰을 환경 변수로만 받는데, 같은 사용자 프로세스가 실행 중인
// 프로세스의 환경을 읽을 수 있고(ps eww) op 캐시 데몬이 그 환경을 물려받아 계속 산다 — 그래서 이 실행기를 둔다.
// kasaterm 앱이 이 프로세스의 코드 서명을 pid 로 확인한 뒤에야 토큰을 써 넣는다.
package main

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"time"

	"github.com/1password/onepassword-sdk-go"
)

type request struct {
	Op    string   `json:"op"`
	Token string   `json:"token"`
	Refs  []string `json:"refs"`
}

type vault struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

type reply struct {
	OK     bool     `json:"ok"`
	Values []string `json:"values,omitempty"`
	Vaults []vault  `json:"vaults,omitempty"`
	Error  string   `json:"error,omitempty"`
	Ref    string   `json:"ref,omitempty"`
}

const version = "1"

func main() {
	out := json.NewEncoder(os.Stdout)
	raw, err := io.ReadAll(io.LimitReader(os.Stdin, 64*1024))
	if err != nil {
		_ = out.Encode(reply{Error: "bad_input"})
		os.Exit(2)
	}
	var req request
	if json.Unmarshal(raw, &req) != nil || req.Token == "" {
		_ = out.Encode(reply{Error: "bad_input"})
		os.Exit(2)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	client, err := onepassword.NewClient(ctx,
		onepassword.WithServiceAccountToken(req.Token),
		onepassword.WithIntegrationInfo("kasaterm", version),
	)
	req.Token = ""
	if err != nil {
		_ = out.Encode(reply{Error: "auth: " + err.Error()})
		os.Exit(1)
	}
	switch req.Op {
	case "vaults":
		list, err := client.Vaults().List(ctx)
		if err != nil {
			_ = out.Encode(reply{Error: "vaults: " + err.Error()})
			os.Exit(1)
		}
		vaults := make([]vault, 0, len(list))
		for _, v := range list {
			vaults = append(vaults, vault{ID: v.ID, Name: v.Title})
		}
		_ = out.Encode(reply{OK: true, Vaults: vaults})
	case "resolve":
		values := make([]string, 0, len(req.Refs))
		for _, ref := range req.Refs {
			value, err := client.Secrets().Resolve(ctx, ref)
			if err != nil {
				_ = out.Encode(reply{Error: err.Error(), Ref: ref})
				os.Exit(1)
			}
			values = append(values, value)
		}
		_ = out.Encode(reply{OK: true, Values: values})
	default:
		_ = out.Encode(reply{Error: "bad_input"})
		os.Exit(2)
	}
}
