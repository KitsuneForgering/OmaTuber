# Pulo ao falar: como PNGTubers movem o avatar com a voz

Data: 2026-10-21 (iteração 33). Pergunta: que movimento faz um PNGTuber parecer vivo sem atrasar a boca, e com que parâmetros, para implementar igual na prévia (egui) e na fonte de navegador do OBS.

## Evidência lida

| Fonte | O que foi lido | Tipo |
| --- | --- | --- |
| PNGTuber Plus, `kaiakairos/PNGTuber-Plus`, branch `master`, licença Unlicense. `main_scenes/main.gd` no commit `3173b21` (2024-03-26); `ui_scenes/selectedSprite/spriteObject.gd`; `autoload/global.gd`; `project.godot`. Acesso por `gh api` em 2026-10-21. | Código-fonte | Primária |
| [veadotube mini 1.3, "bounce with me"](https://olmewe.itch.io/veadotube-mini/devlog/284505/13-bounce-with-me-windows-32-bit-linux-fixes) e [documentação de uso](https://veado.tube/docs/usage/mini/) | Só resultados de busca; páginas não abertas | Secundária, não verificada |
| Fórum itch.io: ["model too bouncy when she talks fix"](https://itch.io/t/3625155/model-too-bouncy-when-she-talks-fix) e ["possible to not bounce"](https://itch.io/t/3167438/possible-to-not-bounce) | Só títulos e trechos da busca | Relato de usuários |
| PNGTuber-Remix: [devlog v1.3.3](https://mudkipworld.itch.io/pngremix/devlog/749355/pngtube-remix-v133) cita controles de "wobble" e "squish" por estado | Trecho da busca | Secundária, não verificada |

## O que o PNGTuber Plus faz (verificado no código)

- `Global` emite `startSpeaking` quando o volume passa do limite e `speaking` muda de falso para verdadeiro (`global.gd`, linhas 94–104): **um pulo por início de fala**, não por sílaba.
- `onSpeak()` (`main.gd`, 243): se o avatar está a menos de 16 px do chão, `yVel = -bounceSlider` (padrão 250 px/s).
- `_process` (`main.gd`, 135–143): integra `y += yVel·0,0166` e `yVel += bounceGravity·0,0166` (padrão 1000 px/s²), prende `y ≤ 0`. O passo fixo de 1/60 s só é correto porque `project.godot` limita `run/max_fps=60`; abaixo de 60 fps, o pulo fica mais lento.
- Com os padrões: altura máxima v²/2g = 31,25 px; tempo no ar 2v/g = **0,5 s**; trajetória parabólica.
- Por sprite (`spriteObject.gd`): balanço senoidal (`wobble`, x/y com frequência e amplitude próprias), rotação e esticamento proporcionais ao deslocamento do arrasto, e a opção `ignoreBounce`.

Hipótese inicial corrigida: eu supunha que o passo fixo deixava o pulo dependente da taxa de quadros em qualquer máquina; o limite de 60 fps no projeto torna isso verdadeiro só quando o jogo cai abaixo de 60.

## Decisão para o OmaTuber

1. **Arco fechado em vez de integração por quadro**: `h(t) = b·4u(1−u)`, `u = t/0,5 s`. É a mesma parábola do PNGTuber Plus, sem depender da taxa de quadros, e pode ser calculada igual em Rust (`layout::bounce_height`) e em JavaScript (`page.html`, `lift`).
2. **Disparo**: a boca passa de fechada para aberta e o avatar já pousou. O PNGTuber Plus permite repetir abaixo de 16 px de altura; aqui só depois de pousar, para não criar uma segunda regra de continuidade nas duas implementações. A diferença é pequena com 0,5 s no ar.
3. **Amplitude**: fração da altura do canvas (independe da resolução), de 0 a 15 %, **desligada por padrão**. Os tópicos de usuários pedindo menos pulo, ou nenhum, indicam que precisa ser ajustável e desligável; ligar sozinho numa atualização mudaria a cena de quem já transmite.
4. **Espaço acima**: a página e a prévia reservam `b × altura` acima do canvas, para o pico não sair do quadro. Uma fonte nova no OBS é criada com essa altura a mais; uma fonte existente mantém o tamanho, e o avatar fica menor em 1/(1+b).
5. Fora desta etapa: balanço contínuo, esticamento e respiração. Ficam como próximos candidatos, se o pulo se mostrar útil.

## Testes executados localmente

- `layout.rs`: arco (0 no início e no fim, `b` no meio), novo pulo só após pousar, pico dentro do quadro com o espaço reservado.
- `tests/view.rs`: o controle **Pulo ao falar** começa desligado e se move pelo teclado (→ = 1 %).
- Chromium sem interface via CDP (`examples/serve_demo` com `DEMO_BOUNCE`, boca alternando a cada 700 ms, janela 512×512, 4 s amostrados a cada ~17 ms):
  - `DEMO_BOUNCE=0.1`: deslocamento máximo de 38,5 px = 0,1 × 385 px do canvas; topo da imagem no pico em 1,4 px (dentro do quadro), em repouso em 39,9 px; 2–3 pulos em 4 s; nenhum erro de JavaScript.
  - `DEMO_BOUNCE=0`: nenhum deslocamento, nenhum espaço reservado; canvas com 425 px (a razão 385/425 ≈ 1/1,1 confere).

## Não verificado

- A fonte de navegador dentro do OBS (CEF) e a fluidez do `requestAnimationFrame` nela.
- A sensação com voz real: se 0,5 s no ar combina com o ritmo de fala de uma pessoa.
- veadotube mini e PNGTuber-Remix: só trechos de busca, sem leitura do código ou da documentação.
