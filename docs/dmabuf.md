# Fonte OmaTuber por DMA-BUF no OBS

Backend Linux experimental: o OmaTuber compõe o avatar numa textura EGL/GBM e
passa o descritor DMA-BUF ao plugin. O plugin registra **OmaTuber (DMA-BUF)**
na lista de fontes do OBS. Não cria dispositivo V4L2, câmera no PipeWire ou
entrada nas listas de câmeras de navegadores e aplicativos de chamada.

Implementação compilada contra OBS 32.2.2. **Renderização e compatibilidade de
GPU ainda precisam de validação em uma sessão real**: o ambiente de execução
desta implementação não expõe `/dev/dri` nem uma conexão com o compositor.
Não há medição de ganho de desempenho.

## Compilar e usar

Requisitos: Linux, toolchain Rust/C, make, pkg-config, headers de OBS >= 32,
EGL, GLES2, GBM e libdrm. Na distribuição Arch, os pacotes correspondentes
normalmente são obs-studio, libglvnd, mesa, libdrm e base-devel. O produtor e o
OBS devem usar uma GPU que suporte ABGR8888 com modificador LINEAR.

```sh
make
omatuber
```

O `make` da raiz compila com `sound,dmabuf`, instala aplicativo, launcher,
ícone e plugin para o usuário. O launcher em `~/.local/bin/omatuber` ativa DMA-BUF automaticamente.
Use `OMATUBER_DMABUF=0 omatuber` para selecionar o backend browser.
`make build` apenas compila; `make run` executa o build local com DMA-BUF;
`make test` roda os testes. Não é necessário sudo.
O plugin é instalado para o usuário em
`~/.config/obs-studio/plugins/omatuber-dmabuf/bin/64bit/omatuber-dmabuf.so`.
Reinicie o OBS após instalar. Este caminho se destina ao OBS nativo; o pacote
Flatpak tem diretórios e permissões diferentes e não foi validado.

No OmaTuber, conecte ao OBS e use **Configurar todas as cenas** para migrar a
coleção atual; veja [como a migração preserva as cenas](obs-scenes.md). Para
configurar somente uma cena, use **Adicionar à cena**. Com
`OMATUBER_DMABUF=1`, essa ação cria ou reutiliza a fonte `omatuber_dmabuf`.
Também é possível adicioná-la manualmente pelo menu de fontes do OBS; nesse
caso, reutilize essa fonte nas demais cenas. Há um consumidor por produtor;
fontes duplicadas independentes não recebem simultaneamente. Use **Adicionar
existente** no OBS para compartilhar uma fonte entre cenas.

O render node padrão é `/dev/dri/renderD128`. Para selecionar outro:

```sh
OMATUBER_DMABUF=1 OMATUBER_DRM_NODE=/dev/dri/renderD129 \
  cargo run --features dmabuf --bin omatuber
```

Os dois processos usam `$XDG_RUNTIME_DIR/omatuber/frames.sock`. O diretório é
privado (0700), o socket é 0600 e ambos conferem o UID do peer. Não há porta de
rede para DMA-BUF. Isso restringe outros usuários; **não isola programas do
mesmo usuário nem identifica criptograficamente o executável OBS**. A fonte
browser anterior continua disponível como backend alternativo; suas rotas
locais de HTTP não foram removidas.

Em execuções diretas via Cargo, DMA-BUF exige `OMATUBER_DMABUF=1`.
O launcher instalado pelo `make` já define essa opção. Erros do produtor
aparecem na interface e stderr; falhas de importação aparecem no log do OBS.
Não há fallback silencioso para envio de frames pela CPU.

Para remover o plugin:

```sh
rm ~/.config/obs-studio/plugins/omatuber-dmabuf/bin/64bit/omatuber-dmabuf.so
```

## Fluxo e sincronização

1. O worker lê o estado publicado pelo aplicativo, com referências apenas aos
   assets da cena atual. Boca, blink, offsets, reações, animações, objetos e
   trajetória do pulo usam as regras atuais de layout.
2. As imagens são decodificadas na CPU. O frame ativo é premultiplicado e
   enviado para uma textura apenas quando muda. Essa etapa contém upload de
   pixels; não se afirma zero cópia para toda a aplicação.
3. GLES2 compõe as camadas num buffer GBM ABGR8888 de um plano, com modificador
   LINEAR explícito. A saída mantém alfa e é limitada a 4096 por lado; a altura
   é ajustada pelo limite de 1080 do projeto.
4. `glFinish` conclui as escritas antes do envio. O socket `SOCK_SEQPACKET`
   transporta um header de 32 bytes e um FD por `SCM_RIGHTS`. O produtor não
   lê os pixels do framebuffer e nunca escreve novamente no buffer enviado.
5. O OBS importa a textura com `gs_texture_create_from_dmabuf`. Pode manter o
   buffer pelo tempo necessário: o backing storage vive enquanto existirem
   referências do kernel/driver. Como não há reutilização pelo produtor, não
   é necessário inventar um atraso fixo para liberar/reutilizar buffers.
6. O plugin pede `NEXT` uma vez e aguarda a resposta antes de pedir outro.
   Mantém apenas a textura atual, além da próxima resposta pendente. Desconexão
   ou resposta ausente por dois segundos limpa a imagem e inicia reconexão.

A primeira versão aloca um novo buffer por frame e espera a GPU de forma
síncrona. Isso privilegia um contrato de ownership simples e verificável.
Não é um pool de buffers com fences explícitas e não há negociação de
modificadores entre GPUs. Otimizar essa parte depende de validar primeiro o
caminho visual e medir alocações, latência e custo de `glFinish` no hardware
real. O cache de imagens decodificadas é limitado a 512 MiB; os temporários de
decodificação e a memória do driver não estão incluídos nesse orçamento.

## Validação

- `make -C native/obs-plugin test`: compilação com warnings como erros, carga da
  biblioteca e registro da fonte com função de registro interceptada (sem
  renderização OBS); passagem de FD, CLOEXEC, rejeição de metadados inválidos,
  pacote sem FD, múltiplos FDs, fechamento e ausência de vazamento de FDs.
- Testes Rust: fallback de camadas, blink, ordem dos objetos, posicionamento,
  produtor único, permissões do socket, limpeza/reinício e erro sem render node.
- ASan/UBSan no transporte: executados com `ASAN_OPTIONS=detect_leaks=0`, pois
  LeakSanitizer não funciona sob o ptrace deste ambiente; a contagem de FDs é
  verificada separadamente pelo teste.

Os checks finais desta implementação passaram: **189 testes Rust aprovados, 2
ignorados**, Clippy com warnings como erros, build padrão sem DMA-BUF e testes
nativos do plugin/transporte. O teste opcional abaixo compila, mas neste
ambiente retorna 77 por ausência do render node:

```sh
make -C native/obs-plugin gpu-test DRM_NODE=/dev/dri/renderD128
```

Ele verifica composição de uma imagem assimétrica, alfa parcial e exportação
do FD. O único readback está nesse teste, como oráculo de pixels; não está no
caminho de produção. Ainda não verifica importação pelo OBS.

Antes de considerar o backend pronto: executar no OBS real; conferir RGBA com
imagem assimétrica (orientação, canais e alfa parcial), animações, pulo e
objetos; matar/reabrir o produtor e OBS; alternar cenas e resolução; medir RSS,
FDs, latência e pacing durante sessão longa. Testes sem GPU não substituem isso.

Referências primárias consultadas em 24/10/2026:
[API gráfica do OBS](https://docs.obsproject.com/reference-libobs-graphics-graphics),
[implementação EGL do OBS](https://github.com/obsproject/obs-studio/blob/master/libobs-opengl/gl-egl-common.c)
e [API de fontes](https://docs.obsproject.com/reference-sources).


## Fonte adicionada, mas sem imagem

Nas propriedades de **OmaTuber (DMA-BUF)**, clique **Atualizar diagnóstico**.
O estado distingue ausência do produtor, GPU incompatível, timeout e falha de
importação. **Frames importados** confirma se os buffers chegaram ao OBS.
Os mesmos estados aparecem nas linhas `[OmaTuber]` do log do OBS.

Se não houver produtor, abra `omatuber` pelo launcher instalado. Em uma execução
via Cargo, é necessário ativar explicitamente `OMATUBER_DMABUF=1` e compilar com
`--features sound,dmabuf`. Confira também se o OmaTuber oferece imagens na prévia.
Uma falha do exportador é exibida de forma persistente no topo do aplicativo;
salvar as preferências não limpa esse erro.


### Falha antiga `cannot create GLES2 context (EGL 0x3000)`

A seleção original exigia `EGL_PBUFFER_BIT`, embora o exportador renderize
somente em FBO e use `EGL_NO_SURFACE`. Um driver GBM pode não oferecer configs
com pbuffer; `eglChooseConfig` pode retornar sucesso com zero correspondências.
Isso explica a assinatura observada sem estabelecer uma falha na criação do
contexto. A seleção agora exige explicitamente `EGL_SURFACE_TYPE=0` e distingue
falhas de bind da API, seleção sem resultados, criação e ativação do contexto.
Um teste de driver simulado cobre esse caso sem depender de GPU.

Um produtor Vulkan também pode alimentar o mesmo plugin, por exportação de
DMA-BUF (`VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf` e
`VK_EXT_image_drm_format_modifier`). Isso exige implementar renderização,
seleção de GPU, layouts, exportação e sincronização do lado Vulkan; a importação
no backend gráfico do OBS continua necessária. Não foi implementado nesta
correção. Referências:
[seleção de configs EGL](https://registry.khronos.org/EGL/sdk/docs/man/html/eglChooseConfig.xhtml),
[contexto sem superfície](https://registry.khronos.org/EGL/extensions/KHR/EGL_KHR_surfaceless_context.txt),
[memória DMA-BUF Vulkan](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_external_memory_dma_buf.html)
e [modificadores DRM Vulkan](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_image_drm_format_modifier.html).
