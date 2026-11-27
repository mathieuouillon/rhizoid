// The differential check's ROOT reader: reads each file listed in `list`
// (one `stem kind` per line) and writes `stem.root.txt`, the rows as
// `rows::dump` prints them: floats as their bits, strings as hex bytes.
//
//   root -b -q -l 'fuzz/differential/read_root.C("list.txt")'

#include <ROOT/RNTupleReader.hxx>
#include <TFile.h>
#include <TROOT.h>
#include <TTree.h>

#include <array>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

#if ROOT_VERSION_CODE < ROOT_VERSION(6, 36, 0)
namespace RNT = ROOT::Experimental;
#else
namespace RNT = ROOT;
#endif

template <typename T, typename B>
static B bits(T v)
{
   B b;
   std::memcpy(&b, &v, sizeof b);
   return b;
}

static std::string hex(const char *s, size_t n)
{
   std::string out;
   char buf[3];
   for (size_t i = 0; i < n; ++i) {
      std::snprintf(buf, sizeof buf, "%02x", static_cast<unsigned char>(s[i]));
      out += buf;
   }
   return out;
}

static void row(FILE *o, long long i, bool flag, int small, int id, unsigned long long big, double x, float y,
                const unsigned short *xyz, const float *hits, size_t n, const std::string &label)
{
   std::fprintf(o, "%lld flag %d\n%lld small %d\n%lld id %d\n%lld big %llu\n", i, flag ? 1 : 0, i, small, i, id, i,
                big);
   std::fprintf(o, "%lld x %016llx\n%lld y %08x\n", i, (unsigned long long)bits<double, std::uint64_t>(x), i,
                bits<float, std::uint32_t>(y));
   std::fprintf(o, "%lld xyz %u %u %u\n%lld hits %zu", i, xyz[0], xyz[1], xyz[2], i, n);
   std::fprintf(o, " ");
   for (size_t k = 0; k < n; ++k)
      std::fprintf(o, k ? " %08x" : "%08x", bits<float, std::uint32_t>(hits[k]));
   std::fprintf(o, "\n%lld label %s\n", i, hex(label.data(), label.size()).c_str());
}

static void tree(const std::string &stem, FILE *o)
{
   auto f = std::unique_ptr<TFile>(TFile::Open((stem + ".root").c_str()));
   auto t = f ? f->Get<TTree>("t") : nullptr;
   if (!t) {
      std::fprintf(o, "ERROR no tree\n");
      return;
   }
   Bool_t flag;
   Char_t small;
   Int_t id, n;
   ULong64_t big;
   Double_t x;
   Float_t y;
   UShort_t xyz[3];
   static std::vector<Float_t> hits(1 << 20);
   static std::vector<char> label(1 << 22);
   t->SetBranchAddress("flag", &flag);
   t->SetBranchAddress("small", &small);
   t->SetBranchAddress("id", &id);
   t->SetBranchAddress("big", &big);
   t->SetBranchAddress("x", &x);
   t->SetBranchAddress("y", &y);
   t->SetBranchAddress("xyz", xyz);
   t->SetBranchAddress("n", &n);
   t->SetBranchAddress("hits", hits.data());
   t->SetBranchAddress("label", label.data());
   for (Long64_t i = 0; i < t->GetEntries(); ++i) {
      std::memset(label.data(), 0, 1024);
      t->GetEntry(i);
      // ROOT's TLeafC holds a C string: it ends at the first NUL.
      std::string s(label.data());
      row(o, i, flag, small, id, big, x, y, xyz, hits.data(), static_cast<size_t>(n), s);
   }
}

static void ntuple(const std::string &stem, FILE *o)
{
   auto reader = RNT::RNTupleReader::Open("t", stem + ".root");
   auto flag = reader->GetView<bool>("flag");
   auto small = reader->GetView<std::int8_t>("small");
   auto id = reader->GetView<std::int32_t>("id");
   auto big = reader->GetView<std::uint64_t>("big");
   auto x = reader->GetView<double>("x");
   auto y = reader->GetView<float>("y");
   auto xyz = reader->GetView<std::array<std::uint16_t, 3>>("xyz");
   auto hits = reader->GetView<std::vector<float>>("hits");
   auto label = reader->GetView<std::string>("label");
   for (auto i : reader->GetEntryRange()) {
      const auto &h = hits(i);
      row(o, static_cast<long long>(i), flag(i), small(i), id(i), big(i), x(i), y(i), xyz(i).data(), h.data(), h.size(),
          label(i));
   }
}

void read_root(const char *list)
{
   std::ifstream in(list);
   std::string stem, kind;
   while (in >> stem >> kind) {
      FILE *o = std::fopen((stem + ".root.txt").c_str(), "w");
      try {
         if (kind == "rntuple")
            ntuple(stem, o);
         else
            tree(stem, o);
      } catch (const std::exception &e) {
         std::fprintf(o, "ERROR %s\n", e.what());
      }
      std::fclose(o);
   }
}
