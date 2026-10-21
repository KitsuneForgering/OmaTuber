# OmaTuber — plano de implementação

## Objetivo

Criar um aplicativo de PNGTuber para Omarchy Linux que use o volume de uma fonte de áudio do OBS Studio para alternar imagens transparentes. O aplicativo configura e acompanha o avatar; uma fonte de navegador do OBS desenha o resultado na cena.

O primeiro resultado utilizável é uma gravação no OBS em que dois PNGs, com a boca fechada e aberta, alternam quando a pessoa fala. A configuração deve ser feita no aplicativo, sem acesso direto da página do avatar ao microfone ou à senha do OBS. Uma pessoa que nunca montou uma cena no OBS deve conseguir chegar a esse resultado seguindo as orientações da própria interface.

## Decisões técnicas

| Peça | Escolha inicial | Motivo |
| --- | --- | --- |
| Aplicativo | Rust com `egui` e `eframe` | Interface desktop em Rust, conforme a escolha do projeto. `eframe` fornece a janela e a integração nativa de `egui`. |
| Comunicação com OBS | `obws`, com a assinatura `InputVolumeMeters` | Usa o medidor da fonte já configurada no OBS e cuida da autenticação do protocolo. |
| Tarefas em segundo plano | Tokio | Mantém a comunicação com o OBS e o servidor local fora do ciclo de desenho da interface. |
| Fonte de navegador | HTML, CSS e JavaScript servidos pelo aplicativo | O OBS recebe uma URL local estável; a página desenha PNGs sobre fundo transparente. |
| Servidor local | `axum`, escutando somente em `127.0.0.1` | Entrega a página, os dois PNGs selecionados e atualizações de estado. |
| Atualizações para a página | Server-Sent Events | O aplicativo envia mudanças de estado; a página reconecta quando o servidor volta. |

Essas bibliotecas são propostas para o primeiro protótipo. Antes de fixar versões, conferir as APIs e registrar as versões resolvidas em `Cargo.lock`. A documentação de [`eframe`](https://docs.rs/eframe/latest/eframe/), [`obws`](https://docs.rs/obws/latest/obws/) e [`axum` para SSE](https://docs.rs/axum/latest/axum/response/sse/) sustenta a viabilidade da composição; ainda não há teste integrado do OmaTuber.

## Fluxo de dados

```text
Microfone → fonte de áudio do OBS → obs-websocket
                                      ↓
                         OmaTuber: medidor e estado
                            ↙                    ↘
                    prévia no egui        servidor em 127.0.0.1
                                               ↓
                                  fonte de navegador no OBS
```

1. Depois de experimentar as imagens na prévia, a pessoa conecta o aplicativo ao OBS. A interface explica onde ativar o servidor WebSocket e encontrar a senha; a porta padrão aparece somente se a conexão precisar de ajuste. A pessoa escolhe uma fonte de áudio entre as entradas retornadas pelo OBS. A seleção deve usar o identificador da fonte, para sobreviver a mudanças de nome.
2. O aplicativo assina `InputVolumeMeters` e extrai somente os dados da fonte escolhida. O [protocolo do OBS](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md) exige a assinatura explícita desse evento; a [implementação](https://github.com/obsproject/obs-websocket/blob/master/src/eventhandler/EventHandler_Inputs.cpp) indica envio a cada 50 ms.
3. Uma pequena máquina de estados decide entre `quieto` e `falando`. Ela terá limiares distintos para abrir e fechar a boca e um tempo mínimo antes de fechar. Os valores iniciais serão ajustados com gravações reais, após conferir a semântica dos níveis entregues pelo OBS.
4. O aplicativo atualiza sua prévia e envia o estado para a página por SSE. A página troca apenas a imagem visível. Os arquivos PNG são servidos por rotas fixas; a URL não aceita caminhos de arquivos arbitrários.
5. Quando a conexão com OBS cai, o aplicativo mostra o erro e põe o avatar em `quieto`. A reconexão é automática. A página mantém o último desenho por um intervalo curto e volta ao estado `quieto` se perder as atualizações.

### Limites de segurança e configuração

- O servidor do OmaTuber escuta apenas no loopback. A porta precisa ser estável para a fonte do OBS continuar funcionando após reiniciar o aplicativo; se estiver ocupada, o aplicativo deve mostrar um erro claro antes de indicar uma URL válida.
- A página não recebe a senha do OBS. Oferecer **Lembrar neste computador** usando um serviço de segredos do Linux, se disponível; nunca gravar a senha no arquivo de preferências ou na URL. Se o serviço não estiver disponível, explicar que a senha será solicitada novamente na próxima sessão. A crate [`keyring`](https://docs.rs/keyring/latest/keyring/v1/index.html) é uma candidata a verificar na implementação.
- O servidor entrega somente os dois PNGs escolhidos. Ao selecioná-los, o aplicativo verifica se são PNGs legíveis e impõe limites de tamanho e dimensões para evitar arquivos acidentais enormes.
- Preferências sem segredo, como caminhos dos PNGs e sensibilidade, podem ser guardadas na pasta de configuração do usuário. A gravação deve ser atômica para não corromper as preferências se o processo terminar durante a escrita.

## Experiência de quem está começando

### Primeiro uso: do desenho à cena

O primeiro uso é uma sequência curta com avanço visível. Cada etapa tem uma ação principal, um resultado observado na prévia e uma explicação curta quando algo impede o avanço. A pessoa pode voltar sem perder as escolhas anteriores.

| Etapa | O que a pessoa vê e faz | Condição para avançar |
| --- | --- | --- |
| 1. Montar o avatar | Solta ou escolhe dois PNGs rotulados **Quieto** e **Falando**. A prévia mostra os estados lado a lado e permite alternar com um botão. Um avatar de exemplo, criado para o projeto, permite experimentar antes de ter arte própria. | As duas imagens são válidas; o exemplo também conta. |
| 2. Conectar o OBS | O aplicativo verifica se o OBS está aberto e tenta a porta local padrão. Se o servidor WebSocket estiver desligado, mostra o caminho no menu do OBS para ativá-lo. Se pedir senha, explica onde copiá-la, com campo oculto e botão de revelar. | Conexão autenticada; erro e correção aparecem na mesma tela. |
| 3. Fazer a voz reagir | Seleciona um microfone pelo nome reconhecível. Um medidor ao vivo mostra se há sinal; a pessoa fala e vê a boca mudar. Uma calibração guiada coleta alguns segundos de silêncio e fala **apenas como níveis**, sem gravar áudio. O ajuste manual fica disponível se a sugestão não funcionar. | O nível muda durante a fala e a prévia reage; se a fonte não estiver ativa na cena, a interface explica como ativá-la. |
| 4. Colocar na cena | Escolhe uma cena existente e aciona **Adicionar avatar ao OBS**. O aplicativo cria a fonte de navegador com a URL local e dimensões adequadas ao avatar, confirma o nome da fonte criada e orienta a posicioná-la na prévia do OBS. | A fonte aparece na cena escolhida; a pessoa pode fazer uma gravação curta para conferir. |

O OBS oferece a requisição [`CreateInput`](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md#createinput) e o cliente Rust [`obws` expõe `Inputs::create`](https://docs.rs/obws/latest/obws/client/struct.Inputs.html). O tipo da fonte é `browser_source`; `url`, `width` e `height` são configurações do [plugin de navegador](https://github.com/obsproject/obs-browser/blob/master/obs-browser-plugin.cpp). **A criação automática ainda precisa ser verificada no OBS local.** Antes do botão criar algo, a interface mostra a cena de destino. Se essa integração falhar, oferece **Copiar URL** com instruções de adição manual. Repetir a ação deve detectar a fonte já criada e oferecer reutilização, evitando duplicatas.

### Tela principal após a configuração

Em uma janela inicial de aproximadamente 1100 × 760 px, a área maior mostra o **PNG atualmente exibido** em um palco quadriculado. Logo abaixo, duas miniaturas mostram os PNGs carregados; a miniatura ativa recebe contorno e o texto **Exibindo agora**. A área lateral acompanha a sequência **OBS → Microfone → Boca → Cena**, cada item com seu próprio estado e ação de reparo. Em larguras menores, as áreas ficam empilhadas e roláveis, sem cortar ações.

![Mockup conceitual da tela principal do OmaTuber, com avatar falando, miniaturas dos dois estados, medidor de voz e estado da cena](ui-mockup.svg)

O [mockup em SVG](ui-mockup.svg) é uma especificação visual, não uma captura do aplicativo implementado. A imagem do personagem nele é apenas um exemplo gráfico. A prévia real deve usar exatamente o PNG selecionado pela pessoa. O fundo quadriculado torna a transparência perceptível; transparência e escala finais devem ser conferidas dentro do OBS, pois a prévia usa outro renderizador.

O cartão **Avatar** troca os PNGs e oferece **Testar boca**. **Microfone** mostra o nome da fonte, o medidor e um controle de **Sensibilidade** com feedback imediato; valores em dBFS e limiares detalhados ficam em **Ajustes avançados**. **No OBS** mostra a cena e a fonte associadas, um botão para abrir o OBS quando possível e **Copiar URL** como alternativa. A senha e a porta ficam em **Conexão**, abertas apenas durante o primeiro uso ou ao reparar a conexão.

### O que a tela precisa mostrar, em tempo real

| Pergunta da pessoa | Indicação permanente | Exemplo de resposta visível |
| --- | --- | --- |
| Qual avatar carreguei? | Miniaturas **Quieto** e **Falando**, nome de cada arquivo e contorno no estado ativo; palco com o PNG real | **Exibindo agora: Falando** |
| A boca está reagindo à voz ou a um teste? | Selo de modo sobre o palco: **AO VIVO**, **TESTE MANUAL** ou **SEM OBS** | **AO VIVO · voz detectada** |
| O microfone está funcionando? | Nome da fonte, medidor animado e marca de sensibilidade no mesmo trilho | **Microfone recebendo som; acima da marca** |
| Por que a boca não abriu? | Frase curta abaixo do estado, baseada na causa conhecida | **Seu som ainda não passou da marca. Ajuste a sensibilidade.** |
| O avatar aparece na transmissão? | Estado da fonte na cena, separado do estado da prévia | **Visível na cena Principal** / **Oculto na cena Principal** / **Ainda não adicionado** |

O cabeçalho não deve dizer **Tudo pronto** apenas porque o aplicativo conectou ao OBS. Esse estado exige dois PNGs válidos, fonte de áudio ativa com níveis recentes e fonte de navegador visível na cena de destino. Quando uma dessas condições é desconhecida, mostrar **Não foi possível verificar** em vez de afirmar que está no ar. Se a cena de destino não é a cena de programa atual, mostrar **Pronto na cena Principal; outra cena está no ar**.

O modo **Teste manual** muda somente a prévia do aplicativo e exibe **Prévia apenas — não altera o OBS**. Ao sair do teste, a tela retorna ao estado calculado pelo áudio. O selo de modo, o contorno da miniatura e o texto sob o palco devem mudar juntos para evitar indicações contraditórias.

### Estados visuais e recuperação

| Situação observável | Avatar no palco | Microfone | OBS/cena | Próxima ação exibida |
| --- | --- | --- | --- | --- |
| Primeiro uso, sem imagens | Área vazia com duas posições para PNG | Aguardando configuração | Aguardando configuração | **Escolher imagens** ou **Usar exemplo** |
| Imagens prontas, OBS fechado | PNG Quieto; teste manual disponível | Sem leitura ao vivo | **OBS fechado** | **Abrir OBS** |
| OBS conectado, fonte sem eventos | PNG Quieto | **Sem sinal desta fonte**; medidor parado | Estado da cena verificado à parte | **Verificar microfone e cena ativa** |
| Fonte silenciada | PNG Quieto | **Microfone silenciado**; medidor marcado como inativo | Estado da cena verificado à parte | **Ativar microfone no OBS** |
| Nível abaixo do limiar | PNG Quieto | Barra se move antes da marca | Estado da cena verificado à parte | **Ajustar sensibilidade**, se a fala não abrir a boca |
| Nível acima do limiar | PNG Falando; miniatura Falando ativa | Barra passa da marca | Estado da cena verificado à parte | Nenhuma ação; mostrar **Sua voz abriu a boca** |
| Fonte de navegador ausente ou oculta | Prévia continua funcionando | Medidor continua funcionando | **Ainda não adicionado** ou **Oculto na cena** | **Adicionar ao OBS** ou **Mostrar fonte no OBS** |

Os estados são independentes: por exemplo, a prévia pode reagir à voz enquanto a fonte de navegador está oculta. Cor reforça a indicação, mas texto, posição e ícone a tornam compreensível sem distinguir cores. Uma animação discreta pode enfatizar a mudança de boca; ela não substitui o PNG nem mascara atrasos da entrada de áudio.

### Comportamento e linguagem da interface

- Usar palavras de tarefa na tela principal: **Quieto**, **Falando**, **Microfone**, **Sensibilidade**, **Adicionar ao OBS**. Termos como `WebSocket`, `dBFS`, `SSE` e porta aparecem apenas nas instruções de reparo ou em **Ajustes avançados**.
- Diferenciar **OBS fechado**, **servidor WebSocket desligado**, **senha incorreta**, **fonte de áudio ausente**, **fonte inativa/silenciada** e **plugin de navegador ausente**. Cada estado mostra uma próxima ação específica. Não apresentar todos como “erro de conexão”.
- Não bloquear a escolha de imagens e a simulação quando o OBS está fechado. Ao reconectar, manter avatar, fonte e sensibilidade já escolhidos.
- Ao substituir um PNG inválido, preservar o PNG anterior e explicar o problema ao lado do seletor. Mostrar nome e dimensões da imagem aceita, sem expor o caminho completo na tela principal.
- Se os dois PNGs tiverem tamanhos diferentes, avisar que a imagem pode “pular” ao falar e mostrar a alternância na prévia. Centralizar ambos na mesma área de desenho; permitir corrigir o alinhamento sem exigir que a pessoa edite os arquivos fora do aplicativo.
- Quando a calibração não separar bem fala e ruído, informar isso e oferecer o ajuste manual. A calibração não deve alterar a fonte de áudio nem os filtros do OBS.
- Suportar navegação por teclado, foco visível, rótulos de texto junto dos ícones e alvos de clique confortáveis. Usar espaçamentos consistentes e contraste legível; o visual pode acompanhar o tema do sistema, sem esconder o estado da aplicação em efeitos decorativos.

### Validação da experiência

Além da gravação técnica, observar pelo menos três pessoas com pouca experiência em OBS executarem as quatro etapas sem orientação oral. Registrar onde hesitam, quais termos não entendem e se conseguem recuperar um erro de conexão. A meta inicial é cada pessoa colocar o avatar numa cena em até dez minutos **depois de ter as imagens disponíveis**. O avatar de exemplo permite medir o fluxo sem depender de arte própria. Esse estudo está proposto, não executado; se a amostra não estiver disponível, uma revisão guiada por tarefas pode revelar problemas, mas não substitui observação de iniciantes.

## Reações e detecção do tipo de som (após a versão 0.1)

Decisão de 20 de outubro de 2026: além de abrir e fechar a boca, o avatar terá reações (rindo, surpreso, irritado, cansado, dormindo) em PNG ou APNG, disparadas pelo tipo de som e não só pelo volume. O medidor do OBS entrega apenas níveis; distinguir risada de batida na mesa exige ler o áudio. Essa é a exceção prevista no risco do medidor: a boca continua usando `InputVolumeMeters`, e só a detecção de som lê o microfone pelo PipeWire.

### Organização das imagens

| Categoria | Papel | Exemplo no modelo SrRaposaTheProgrammer |
| --- | --- | --- |
| Base | Boca × olhos; visível quando nada mais toca | Quieto, Falando, Piscando, Falando + piscando |
| Base alternativa | Outro conjunto completo, com animação de entrada | Café, com a sequência de beber |
| Reações | Substituem o avatar por um tempo | Rindo, Surpreso, Irritado, Cansado, Dormindo, Sarcástico, Focado |
| Efeitos | APNGs fora do avatar, em pontos de overlay | !, ?, Zzz, marca de raiva, brilho |
| Roupas | Bases alternativas, opcionais | Moletom, camisa |

Cada reação define gatilho, destino (o avatar ou um ponto de overlay), prioridade, comportamento ao terminar (voltar à base, repetir enquanto a condição durar ou ficar até o próximo atalho) e um efeito opcional que toca junto. Empates de prioridade favorecem a reação mais recente. Toda reação também responde a `omatuber reagir <nome>`, para ser associada a um `bind` do Hyprland. Cada ponto de overlay é uma fonte de navegador com rota fixa (`/overlay/<ponto>`), posicionada pela pessoa no OBS. Repetir uma reação precisa reiniciar o APNG desde o primeiro quadro.

### Detecção

Classificador principal (decisão de 20 de outubro de 2026): **Naive Bayes gaussiano treinado com a voz da própria pessoa**, em Rust puro (`src/sound.rs`). A cada 0,25 s, uma janela de 0,5 s vira 36 características: forma do espectro em 24 faixas mel (relativa à média da janela), volume (média, desvio, amplitude e maior subida), cruzamentos por zero, centroide e planicidade espectral, ritmo do envelope de 4 a 10 Hz (risada tem ~4–6 sílabas por segundo), fração de quadros com tom de voz e altura média da voz. Janelas abaixo de −50 dBFS são silêncio e não são classificadas. As classes têm priores iguais; como as características são correlacionadas, as log-verossimilhanças são divididas por uma temperatura (6) antes de virarem probabilidades. Uma reação dispara quando a mesma classe é a mais provável, com p ≥ 0,7, em 2 janelas seguidas; `fala` e `outros` competem mas nunca disparam. Vantagens: alguns KB, sem modelo baixado, janela menor que a do YAMNet e personalizado. Limite: confunde sons que a pessoa não ensinou, por isso a classe `outros` (teclado, cliques, música) é parte do treino.

Comparação: YAMNet, rodando os mesmos clipes por `tools/yamnet/compare.py`. Descrição do modelo:

[YAMNet](https://github.com/tensorflow/models/tree/master/research/audioset/yamnet), classificador de eventos sonoros treinado no AudioSet, com 521 classes, entrada de 16 kHz mono e cerca de 3,7 milhões de pesos. A [lista de classes](https://github.com/tensorflow/models/blob/master/research/audioset/yamnet/yamnet_class_map.csv) inclui as usadas aqui: Laughter, Giggle, Snicker, Belly laugh e Chuckle (risada); Gasp e Screaming (susto); Shout, Yell e Bellow (grito); Sigh e Groan (suspiro). Não há classe de bocejo nem de surpresa falada; **Dormindo** continua por silêncio longo.

- **Atraso:** o modelo precisa de pelo menos 975 ms de áudio para a primeira saída; a reação chega de 0,5 a 1 s depois do som. Aceitável para risada e suspiro; para susto, avaliar se o atraso tira o efeito.
- **Precisão:** o README publica mAP balanceado de 0,306 no conjunto de avaliação do AudioSet. Isso não diz como o modelo se sai com a voz e o microfone de uma pessoa; a decisão depende do teste abaixo.
- **Execução em Rust:** converter o modelo para ONNX e rodar em CPU, numa tarefa separada da interface. Verificar `tract-onnx` (Rust puro) e `ort` (ONNX Runtime) quanto a compatibilidade e uso de CPU. Confirmar a licença dos pesos antes de distribuí-los.
- **Captura:** ler o mesmo microfone pelo PipeWire, com reamostragem para 16 kHz. Filtros do OBS, como supressão de ruído, não se aplicam a essa leitura.
- **Privacidade:** o áudio passa pelo modelo na memória e é descartado; nada é gravado nem enviado. A interface diz isso ao lado do seletor de microfone. A detecção começa desligada e é ligada pela pessoa.

### Prova antes de construir

Gravar, com consentimento e só localmente, alguns minutos da própria pessoa falando, rindo, arfando, gritando e suspirando, mais ruídos comuns (teclado, batidas, música de fundo). Para cada som, medir em quantos trechos o modelo acerta e quantos disparos falsos ocorrem em 10 minutos de fala comum. **Avançar quando** risada e grito forem reconhecidos na maioria dos casos sem disparos falsos frequentes; caso contrário, manter essas reações por atalho e rever o modelo.

## Etapas e critérios de conclusão

### 1. Prova da integração com OBS

- Verificar se o OBS local oferece a fonte de navegador. Nesta máquina, OBS 32.2.2, `obs-websocket` e o pacote Arch [`obs-studio-plugin-browser`](https://archlinux.org/packages/extra/x86_64/obs-studio-plugin-browser/) estão instalados; falta validar a fonte numa cena real.
- Conectar um cliente Rust ao `obs-websocket` com autenticação e listar as fontes de áudio.
- Registrar amostras dos eventos de volume durante silêncio, fala e microfone silenciado, sem gravar áudio. Confirmar qual valor do evento é adequado para a decisão de abrir a boca.
- **Avançar quando:** a fonte escolhida puder ser identificada e seus eventos chegarem continuamente em uma sessão real do OBS.

### 2. Aplicativo e imagens

- Criar a janela com `eframe`, escolha de dois PNGs, prévia, sensibilidade e mensagens de erro compreensíveis. Acrescentar o fluxo inicial em etapas, um avatar de exemplo com licença definida e os estados visuais do [mockup](ui-mockup.svg).
- Implementar a máquina de estados separada da interface, com testes para silêncio, fala perto do limiar, oscilações e perda da fonte.
- Fazer os indicadores de modo, miniatura ativa, PNG no palco, medidor e texto de causa consumirem o mesmo estado observado pela interface. O teste manual deve declarar que afeta somente a prévia.
- **Avançar quando:** imagens e estado mudarem corretamente na prévia, inclusive no modo de simulação, o primeiro uso puder começar com o OBS fechado e nenhuma indicação **AO VIVO** permanecer após perda de conexão.

### 3. Fonte de navegador

- Servir a página transparente e os PNGs por HTTP local; enviar o estado por SSE.
- Criar a fonte de navegador na cena escolhida após o clique da pessoa; oferecer a URL e a adição manual se o OBS recusar a criação. A [fonte de navegador do OBS](https://obsproject.com/kb/browser-source) aceita URL e fundo transparente.
- **Avançar quando:** a fonte aparecer sobre outra imagem no OBS, sem retângulo opaco, acompanhar as mudanças feitas no aplicativo e não ser duplicada quando a ação for repetida.

### 4. Verificação de uso real

- Gravar no OBS sessões curtas com silêncio, fala normal, pausas, microfone silenciado, troca de fonte e reinício do aplicativo.
- Inspecionar visualmente as gravações e medir o intervalo entre início da fala e abertura da boca. Usar 250 ms como limite inicial para 95% das transições observadas; revisar esse limite se a cadência de 50 ms do medidor ou o processamento do OBS o tornar inadequado, registrando o motivo.
- Conferir que 30 segundos de silêncio não produzam aberturas falsas e que a fonte se recupere após reiniciar o aplicativo.
- Conferir visualmente as combinações da tabela **Estados visuais e recuperação**. Em especial, ocultar a fonte no OBS enquanto a voz continua ativa: a prévia deve continuar reagindo, e o estado **Visível na cena** deve desaparecer.
- Executar as tarefas de primeiro uso e reparo com iniciantes, conforme a seção de validação da experiência; revisar os pontos de hesitação antes de considerar a interface pronta.
- **Concluir a versão 0.1 quando:** a gravação mostrar transparência correta, resposta estável à fala e recuperação de uma desconexão sem refazer a configuração da cena, e o fluxo guiado permitir colocar o avatar no OBS sem recorrer a instruções externas.

## Riscos que podem mudar o plano

| Risco | Verificação decisiva | Resposta prevista |
| --- | --- | --- |
| A fonte de navegador não está disponível ou falha na instalação local | Criar uma fonte vazia no OBS após instalar o pacote correspondente | Resolver o empacotamento do OBS antes de desenvolver a sobreposição. |
| O medidor não representa a fala como esperado, especialmente após filtros ou mudo | Comparar eventos com o medidor e com uma gravação do OBS | Ajustar a fonte ou o valor lido; considerar captura via PipeWire apenas se o medidor do OBS se mostrar inadequado. |
| A cadência dos eventos produz animação perceptivelmente atrasada | Medir transições numa gravação, conforme a etapa 4 | Ajustar limiares e tempo de fechamento; avaliar outra origem de áudio se persistir. |
| O aplicativo fecha e a fonte perde a URL | Reiniciar o aplicativo com a fonte já configurada no OBS | Manter porta estável e reconexão automática; mostrar estado de espera na interface. |
| A criação automática da fonte falha ou cria duplicatas | Executar a ação duas vezes e repetir após renomear ou trocar de cena | Detectar fontes associadas, oferecer reutilização e manter a opção de adição manual. |
| O classificador de som erra com a voz da pessoa ou dispara com ruídos | Medir acertos e disparos falsos em gravações próprias, conforme a prova da detecção | Ajustar a confiança mínima por som; deixar a reação apenas por atalho se não houver ajuste aceitável. |
| O fluxo parece claro para quem já conhece OBS, mas trava um iniciante | Observar as quatro tarefas com pessoas pouco experientes | Reescrever instruções e mudar a ordem das ações nos pontos em que houver hesitação. |

## Estado da validação

Este plano se baseia em documentação oficial de OBS e das bibliotecas Rust, na inspeção do repositório (que começou apenas com o README) e numa checagem de pacotes instalados nesta máquina em 20 de outubro de 2026. **Nenhuma etapa de integração ou gravação foi executada ainda.** A primeira prova com o OBS é o ponto de decisão antes de investir na interface completa.

### Registro de execução

**2026-10-20, iteração 1.** Verificado na fonte, sem OBS em execução:

- `obs-websocket` (`src/utils/Obs_VolumeMeter.cpp`, master): cada canal de `inputLevelsMul` é `[RMS × volume, pico × volume, pico bruto]`, em multiplicador linear. Mudo zera os dois primeiros; o terceiro ignora mudo e volume e não serve para a boca. Os valores vêm do último bloco de áudio antes do tick de 50 ms, não de uma média do intervalo, e zeram após 0,3 s sem áudio. Só fontes ativas (`obs_source_active`) aparecem no evento: um microfone fora da cena atual some do fluxo.
- `obws` 0.15.0: `InputVolumeMeter` expõe apenas `inputName`, não `inputUuid`. A seleção continua guardando o UUID; o nome correspondente é resolvido por `GetInputList` e atualizado pelo evento `InputNameChanged`.
- Configuração local do `obs-websocket`: `server_enabled: false`. É preciso ativar o servidor no OBS (Ferramentas → Configurações do servidor WebSocket) antes da etapa 1. Nesse momento da iteração, o pacote `obs-studio-plugin-browser` ainda estava ausente.

Implementado: `src/meter.rs` (conversão para dBFS usando só valores após volume/mudo), `src/mouth.rs` (histerese, retenção e expiração, com testes) e `src/bin/obs-probe.rs` (`list` e `record <uuid> [s]`, saída CSV). Os limiares padrão (-30/-38 dBFS, retenção de 150 ms) são provisórios. A sonda ainda não rodou contra um OBS real.

**2026-10-20, iteração 2.** OBS aberto e `obs-studio-plugin-browser` instalado, mas a porta 4455 não escuta: o servidor WebSocket continua desligado, e a etapa 1 segue pendente.

Etapa 2, parcial: janela `eframe` 0.37 (a API agora usa `App::ui(&mut Ui, …)`, não `update`) com seções **Áudio** e **Avatar**. `src/avatar.rs` lê cada PNG uma vez, rejeita arquivos acima de 16 MiB, formatos que não sejam PNG e imagens acima de 4096 px por lado antes de decodificar; os mesmos bytes serão servidos na etapa 3. O seletor usa `rfd` pelo xdg-desktop-portal numa thread separada. A sensibilidade é um controle único (fechar = abrir − 8 dB), com medidor e marcas dos dois limiares. A simulação alimenta a máquina de estados a cada 50 ms, como o OBS. Verificado: compilação sem avisos do clippy, 26 testes e abertura da janela no Hyprland. Não verificado: escolha de arquivo e simulação por interação real. Faltam as seções **Conexão OBS** e **Saída OBS**.

**2026-10-20, revisão de UI/UX.** O plano passou a definir um primeiro uso guiado, prévia antes da conexão com OBS, linguagem voltada à tarefa, calibração assistida, criação da fonte na cena escolhida e critérios de observação com iniciantes. Essas são decisões de projeto; a janela atual ainda mostra as seções técnicas parciais descritas na iteração 2.

**2026-10-20, revisão dos estados visuais.** Adicionado o [mockup conceitual](ui-mockup.svg) e uma matriz de estados para tornar visíveis o PNG carregado, a imagem exibida agora, a origem da prévia, o motivo da posição da boca, o sinal do microfone e a visibilidade da fonte no OBS. O mockup foi renderizado e inspecionado; a interface real ainda precisa implementar e validar esses comportamentos.

**2026-10-20, iteração 3.** A configuração do `obs-websocket` agora tem `server_enabled: true`, mas o OBS estava fechado; a sonda ainda não rodou contra ele.

Etapa 3, parcial: `src/server.rs` com `axum` 0.8 em `127.0.0.1:39017`. Rotas fixas `/`, `/closed.png`, `/open.png` e `/events`; outros caminhos dão 404. Os PNGs são os bytes validados na escolha, sem cópia (`Arc<[u8]>`), com `Cache-Control: no-store` e `?rev=` para invalidar o cache da página. O SSE envia imagens e estado ao conectar, cada mudança e um reenvio do estado por segundo. A página (`src/page.html`) fecha a boca em erro de conexão ou após 3 s sem mensagens. O app só mostra a URL depois de conseguir a porta; se estiver ocupada, mostra o erro e um botão para tentar de novo.

Verificado: 34 testes (rotas, 404 fora das rotas fixas, sequência SSE com relógio simulado, porta ocupada, bind só em loopback) e execução real (página 200, `/events` com estado inicial e reenvio a cada segundo, segunda instância aberta sem derrubar a primeira). Não verificado: a mensagem de porta ocupada na tela, a página dentro do OBS e a transparência.

**2026-10-20, iteração 4.** OBS fechado; a conexão real continua sem teste.

Seção **Conexão OBS** implementada em `src/obs.rs`. A máquina de estados saiu da interface para uma tarefa Tokio, porque o `eframe` não chama `App::ui` com a janela oculta e a fonte do OBS precisa continuar animando com o app minimizado. A tarefa recebe comandos da interface (conectar, desconectar, escolher fonte, sensibilidade, simulação), publica o estado para a interface e para o servidor e verifica o estado a cada 50 ms. Reconexão automática com espera de 1 s dobrando até 10 s; senha recusada (código 4009) não repete as tentativas. A lista de fontes é refeita em `InputCreated`, `InputRemoved` e `InputNameChanged`.

Defeito encontrado no `obws` 0.15 (`events.rs`, `EventStream::poll_next`): um `Lagged` do canal broadcast é convertido em `None` e, com `fuse()`, encerra o stream de eventos para sempre com a conexão ainda aberta. Mitigação: o fim do stream é tratado como desconexão e dispara a reconexão, e o buffer de eventos subiu de 100 para 1024.

Verificado: 38 testes, incluindo recusa de conexão com nova tentativa agendada e um teste de integração por TCP real em que a simulação chega à página como `data: open` e volta a `data: closed`. Não verificado: conexão, autenticação e medidores com o OBS real; interface desta versão não foi aberta.

**2026-10-20, iteração 5.** OBS fechado.

Preferências em `src/prefs.rs`: porta do OBS, UUID da fonte de áudio, sensibilidade e caminhos dos PNGs, em `$XDG_CONFIG_HOME/omatuber/prefs.json` (ou `~/.config`). A senha não é gravada. A escrita é atômica: arquivo temporário na mesma pasta, `fsync`, `rename` e `fsync` da pasta. Arquivo ausente usa os padrões; arquivo inválido gera um aviso na janela e não é sobrescrito até a pessoa mudar alguma configuração. O app salva quando algo muda, exceto durante o arraste de um controle. Na abertura, os PNGs salvos são recarregados e validados; um arquivo ausente mostra erro, mas o caminho continua salvo.

Verificado: 43 testes (5 novos para ida e volta, campos ausentes, arquivo corrompido, sobrescrita e ausência de arquivo temporário). Não verificado: o fluxo de salvar e reabrir pela interface.

**2026-10-20, iteração 6.** OBS fechado. Interface refeita depois da crítica de que estava ruim de usar: a janela anterior era uma coluna de seções na ordem dos módulos, com jargão ("dBFS"), prévia de 256 px de uma imagem só e simulação por caixa de seleção mais controle deslizante.

Nova organização (`src/view.rs`, sem E/S, devolve ações para `main.rs`): faixa de estado no topo (OBS, microfone, copiar URL), com cor por situação; painel de conexão que abre no início da sessão e em erro e se recolhe ao conectar; as duas imagens lado a lado sobre fundo xadrez, com a imagem ativa destacada, clique ou arraste para trocar; medidor com a linha de sensibilidade arrastável sobre a própria barra; botão "Segure para testar". Cores lidas do tema ativo do Omarchy em `~/.local/state/omarchy/current/theme/colors.toml` (o caminho `~/.config/omarchy/current/theme`, citado antes sem conferir, não existe nesta máquina).

Verificação visual por renderização fora da tela com `egui_kittest` (`cargo run --example render_view -- <pasta>`), em quatro cenas: primeira abertura, falando, janela estreita e erros. As capturas mostraram e permitiram corrigir: rótulo "Microfone" cortado na janela estreita; mensagem de erro longa alargando o conteúdo além da janela; borda de "ativo" em cartão vazio; o glifo "→", ausente da fonte padrão; e quatro controles sem nome acessível (senha, porta, seletor de fonte, faixas de estado). Verificado: 51 testes, 6 deles de interação pela árvore de acessibilidade. Não verificado: janela real, arrastar arquivo no Wayland (posição do cursor durante o arraste), botão de teste pressionado e arraste da linha de sensibilidade.

**2026-10-20, iteração 7.** OBS fechado. Verificação parcial da etapa 3 num navegador real, usando o Chromium do sistema em modo sem interface (a fonte de navegador do OBS também usa Chromium, via CEF, mas versão e configuração diferem).

`examples/serve_demo.rs` serve a página com dois PNGs gerados e alterna a boca por tempo. Captura direta com `--default-background-color=00000000`: cantos com alfa 0, rosto opaco, sem retângulo de fundo e sem barras de rolagem. Troca pelo SSE: o `--screenshot` do Chromium captura no evento `load`, antes de qualquer atualização (a primeira tentativa deu duas imagens iguais por isso, não por falha da página). Com uma página externa que segura o `load` por 3 s e alternância aos 2 s, a captura mostrou a boca aberta: pixels escuros da boca foram de 774 para 3701. Não verificado: a página dentro do OBS, com sua escala e transparência.

**2026-10-20, design de reações.** Definidos, no canvas de UI/UX, a biblioteca de reações por categoria, os pontos de overlay no OBS e a tela de detecção de som, usando o modelo SrRaposaTheProgrammer. Decidido detectar o tipo de som pelo PipeWire com um classificador local (seção **Reações e detecção do tipo de som**). Nada disso foi implementado; a lista de classes do YAMNet foi conferida no repositório oficial, mas o modelo não rodou nesta máquina.

**2026-10-20, prova do classificador de som.** Decidido usar Naive Bayes treinado com a voz da pessoa e manter o YAMNet só como comparação (seção **Detecção**). Implementado atrás da feature `sound` do Cargo, sem afetar o build padrão: `src/sound.rs` (características, classificador, validação "deixa um clipe de fora" e detector com confirmação), `src/capture.rs` (captura pelo PipeWire pedindo F32LE 16 kHz mono; o PipeWire faz a reamostragem) e `src/bin/sound-probe.rs` (`levels`, `record`, `train`, `live`, `classify`). Os exemplos ficam em `~/.local/share/omatuber/sounds/<classe>/` como WAV 16 kHz e não saem do computador. O YAMNet foi exportado para ONNX a partir do código e dos pesos oficiais (`tools/yamnet/export.py`; só a rede, com o log-mel calculado em NumPy por `tools/yamnet/compare.py`); ONNX e TensorFlow diferem em no máximo 2×10⁻⁶ nas pontuações, e o log-mel em NumPy difere do TensorFlow em até 1,9×10⁻⁴. O modelo exportado fica em `models/`, fora do git.

Verificado: 9 testes novos (filtros mel, porta de silêncio, altura de voz numa dente de serra de 200 Hz, ritmo de ruído pulsado a 5 Hz, separação tom × chiado, JSON, confirmação do detector), clippy limpo com e sem a feature e 53 testes passando. A captura funcionou pelo microfone embutido, com o formato confirmado no `pw-dump`; o nível medido (cerca de −10 dB RMS, picos em 0 dB) bate com o `pw-record`, ou seja, esse microfone está saturando. O fluxo completo rodou com dados de teste (7 frases do espeak-ng, 6 ruídos sintéticos e 3 clipes ambientes gravados pelo `record`): Bayes acertou 15 de 16 clipes na validação "deixa um de fora" e classificou como fala uma frase fora do treino; o YAMNet também acertou 15 de 16 (ruído ambiente de um clipe virou fala). **Não verificado:** risada, susto, grito e suspiro reais; esses números dizem só que o encadeamento funciona. A próxima etapa é gravar a voz da pessoa e comparar os dois classificadores nos mesmos clipes.
