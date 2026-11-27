// A line-at-a-time front end to bergamot-translator.
//
// The stock `bergamot` CLI reads all of stdin before it translates anything,
// so it cannot serve a dictation one sentence at a time. This one answers each
// input line with one output line, flushed, and says "ready" once the model is
// built so the caller can tell loading from translating.
//
//   myna-bergamot --model-config-paths model.yml
#include <future>
#include <iostream>
#include <string>

#include "translator/parser.h"
#include "translator/response.h"
#include "translator/response_options.h"
#include "translator/service.h"

int main(int argc, char *argv[]) {
  using namespace marian::bergamot;
  ConfigParser<AsyncService> parser("myna-bergamot", /*multiOpMode=*/false);
  parser.parseArgs(argc, argv);
  const auto &config = parser.getConfig();

  AsyncService service(config.serviceConfig);
  auto options = parseOptionsFromFilePath(config.modelConfigPaths.front());
  std::shared_ptr<TranslationModel> model = service.createCompatibleModel(options);
  ResponseOptions responseOptions;

  std::cout << "ready" << std::endl;

  std::string line;
  while (std::getline(std::cin, line)) {
    std::string out;
    if (line.find_first_not_of(" \t\r") != std::string::npos) {
      std::promise<Response> promise;
      std::future<Response> future = promise.get_future();
      service.translate(
          model, std::move(line),
          [&promise](Response &&response) { promise.set_value(std::move(response)); },
          responseOptions);
      out = future.get().target.text;
      for (char &c : out) {
        if (c == '\n' || c == '\r') c = ' ';
      }
    }
    std::cout << out << std::endl;
  }
  return 0;
}
